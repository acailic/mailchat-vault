//! Chat export format detection and import for the generic "import chat
//! export" path: the picked file's contents decide which source parses it.
//!
//! [`import`] orchestrates the whole flow — detect, collect, extract
//! media (when enabled and a data dir is given), then ingest with media
//! backfill — so the Tauri shell stays thin and the flow is testable
//! without the app.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discord::{is_discord_export, is_discord_export_dir, DiscordSource, DISCORD_SOURCE};
use crate::ingest::{ingest_collected, IngestSummary};
use crate::media::{self, MediaSummary};
use crate::signal::{is_signal_export, SignalSource, SIGNAL_SOURCE};
use crate::storage::Storage;
use crate::telegram::{is_telegram_export, TelegramSource};
use crate::whatsapp::{is_whatsapp_export, WhatsappSource};
use crate::{Error, Result};

/// Recognized chat export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatFormat {
    WhatsApp,
    Telegram,
    Signal,
    Discord,
}

/// Detect which chat export format `bytes` hold, if any.
///
/// Discord wins when the JSON carries a `messages` array plus a
/// `channel`/`guild` object (Telegram exports also have a top-level
/// `messages` array, so the Discord shapes must be checked first);
/// a top-level JSON array of Signal Desktop message objects is a
/// Signal conversation export; otherwise a Telegram `messages` array
/// decides, then a WhatsApp header within the first lines. Anything
/// else returns `None`.
pub fn detect_chat_format(bytes: &[u8]) -> Option<ChatFormat> {
    if is_discord_export(bytes) {
        return Some(ChatFormat::Discord);
    }
    if is_telegram_export(bytes) {
        return Some(ChatFormat::Telegram);
    }
    if is_signal_export(bytes) {
        return Some(ChatFormat::Signal);
    }
    if is_whatsapp_export(bytes) {
        return Some(ChatFormat::WhatsApp);
    }
    None
}

/// Outcome of a chat export import: the ingest summary plus (when media
/// extraction ran) how the media pass went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImportOutcome {
    pub summary: IngestSummary,
    pub media: Option<MediaSummary>,
}

/// Import a chat export: a WhatsApp `.txt` export, a Telegram
/// `result.json` (a directory containing `result.json` works too), a
/// Signal conversation `.json` (e.g. from `sigtop export-messages -f
/// json`, one file per conversation), or a Discord Data Package export —
/// a single channel `messages.json`, the package's `messages/` directory,
/// or the package root. The format is detected from the file contents
/// (directory picks must be recognizable as a Discord Data Package).
///
/// Media extraction runs when `extract_media` is set and `data_dir` (the
/// app data dir that will hold the `media/` store) is given: export files
/// are copied into `data_dir/media/` hash-named and linked from each
/// message's `raw_json`; messages already in the archive get their links
/// backfilled, so re-importing an export with media enriches it. Pass
/// `extract_media = false` (or `data_dir = None`) to import text only.
/// Signal and Discord exports record attachments in `raw_json` only —
/// media extraction is not implemented for Signal or Discord yet.
pub fn import(
    storage: &Storage,
    picked: &Path,
    extract_media: bool,
    data_dir: Option<&Path>,
) -> Result<ChatImportOutcome> {
    let file = resolve_export_file(picked)?;
    // A directory pick is a Discord Data Package by this point
    // (resolve_export_file returns the dir itself only in that case);
    // there is no single file to sniff, so the format is decided here.
    let format = if file.is_dir() {
        ChatFormat::Discord
    } else {
        let bytes = std::fs::read(&file)?;
        detect_chat_format(&bytes).ok_or_else(|| {
            Error::ChatImport(format!(
                "unrecognized chat export format: {} (expected a WhatsApp .txt export, a Telegram result.json, a Signal conversation .json, or a Discord messages.json)",
                file.display()
            ))
        })?
    };

    let collected = match format {
        ChatFormat::Telegram => {
            use crate::ingest::Source;
            TelegramSource::new(&file).collect()?
        }
        ChatFormat::Signal => {
            use crate::ingest::Source;
            SignalSource::new(&file).collect()?
        }
        ChatFormat::WhatsApp => {
            use crate::ingest::Source;
            WhatsappSource::new(&file).collect()?
        }
        ChatFormat::Discord => {
            use crate::ingest::Source;
            DiscordSource::new(&file).collect()?
        }
    };

    let mut media_summary = None;
    let mut messages = collected.messages;
    if extract_media {
        if let Some(data_dir) = data_dir {
            let export_root = file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            match format {
                ChatFormat::Telegram => {
                    let mut summary = MediaSummary::default();
                    media::extract_telegram(&mut messages, &export_root, data_dir, &mut summary)?;
                    media_summary = Some(summary);
                }
                ChatFormat::WhatsApp => {
                    let mut summary = MediaSummary::default();
                    media::extract_whatsapp(&mut messages, &export_root, data_dir, &mut summary)?;
                    media_summary = Some(summary);
                }
                // Signal and Discord attachments stay recorded in `raw_json`
                // only — media extraction is not implemented for them yet.
                ChatFormat::Signal => {}
                ChatFormat::Discord => {}
            }
        }
    }

    let source_name = match format {
        ChatFormat::Telegram => crate::telegram::TELEGRAM_SOURCE,
        ChatFormat::Signal => SIGNAL_SOURCE,
        ChatFormat::WhatsApp => crate::whatsapp::WHATSAPP_SOURCE,
        ChatFormat::Discord => DISCORD_SOURCE,
    };
    let summary = ingest_collected(
        storage,
        source_name,
        crate::ingest::Collected {
            messages,
            failed: collected.failed,
        },
        media_summary.as_mut(),
    )?;
    Ok(ChatImportOutcome {
        summary,
        media: media_summary,
    })
}

/// Resolve the picked path to the export file: `result.json` inside a
/// picked directory, the picked directory itself when it is a Discord
/// Data Package, or the picked file itself.
fn resolve_export_file(picked: &Path) -> Result<PathBuf> {
    if picked.is_dir() {
        let result_json = picked.join("result.json");
        if result_json.is_file() {
            return Ok(result_json);
        }
        if is_discord_export_dir(picked) {
            return Ok(picked.to_path_buf());
        }
        return Err(Error::ChatImport(format!(
            "unrecognized chat export format: {} (expected a directory containing a Telegram result.json or a Discord Data Package)",
            picked.display()
        )));
    }
    Ok(picked.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHATSAPP_BYTES: &[u8] = b"[1.11.2026., 09:00:05] Marko: Zdravo ekipa!\n";

    fn telegram_bytes() -> Vec<u8> {
        br#"{"name": "Arhiva", "type": "private_group", "id": 42, "messages": [{"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "from": "Marko", "text": "hi"}]}"#
            .to_vec()
    }

    fn discord_bytes() -> Vec<u8> {
        br#"{"channel": {"id": "900000000000000101", "name": "general"}, "guild": {"id": "800000000000000001", "name": "Ekipa"}, "messages": [{"id": "1700000000000000001", "type": "Default", "timestamp": "2026-10-31 18:00:05.123+00:00", "content": "Zdravo!"}]}"#
            .to_vec()
    }

    #[test]
    fn detects_whatsapp_txt() {
        assert_eq!(
            detect_chat_format(WHATSAPP_BYTES),
            Some(ChatFormat::WhatsApp)
        );
    }

    #[test]
    fn detects_telegram_json() {
        assert_eq!(
            detect_chat_format(&telegram_bytes()),
            Some(ChatFormat::Telegram)
        );
    }

    #[test]
    fn detects_signal_json_array() {
        let signal = br#"[
            {
                "id": "b1f2e3d4",
                "type": "incoming",
                "conversationId": "conv-1",
                "source": "+385912345678.2",
                "sent_at": 1698832800000,
                "timestamp": 1698832800000,
                "body": "Zdravo ekipa!"
            }
        ]"#;
        assert_eq!(detect_chat_format(signal), Some(ChatFormat::Signal));
    }

    #[test]
    fn detects_discord_json() {
        // Both the full guild shape and a bare DM channel file (no guild
        // key) must be detected.
        assert_eq!(
            detect_chat_format(&discord_bytes()),
            Some(ChatFormat::Discord)
        );
        let dm = br#"{"channel": {"id": "411116245892313088"}, "messages": []}"#;
        assert_eq!(detect_chat_format(dm), Some(ChatFormat::Discord));
    }

    #[test]
    fn messages_array_without_channel_or_guild_is_telegram() {
        // The two JSON formats share a top-level messages array; only
        // the channel/guild objects make it Discord.
        assert_eq!(
            detect_chat_format(&telegram_bytes()),
            Some(ChatFormat::Telegram)
        );
        assert_eq!(
            detect_chat_format(br#"{"messages": []}"#),
            Some(ChatFormat::Telegram)
        );
        assert_eq!(detect_chat_format(br#"{"channel": {}}"#), None);
    }

    #[test]
    fn other_files_are_none() {
        for bytes in [
            &b"just some plain text without stamps"[..],
            b"{\"name\": \"not an export\"}",
            b"[not a stamp] random bracket text\nmore\n",
            b"",
        ] {
            assert_eq!(detect_chat_format(bytes), None, "{:?}", bytes);
        }
    }
}
