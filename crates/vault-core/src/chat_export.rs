//! Chat export format detection and import for the generic "import chat
//! export" path: the picked file's contents decide which source parses it.
//!
//! [`import_bounded`] orchestrates the whole flow — detect, stream,
//! batch-commit (extracting media when enabled and a data dir is given) —
//! so the Tauri shell stays thin and the flow is testable without the app.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bounded::{
    import_id_for, BoundedOutcome, BoundedRun, IngestSink, MediaKind, StreamSource,
};
use crate::discord::{
    is_discord_export, is_discord_export_dir, is_discord_export_reader, DiscordSource,
};
use crate::ingest::IngestSummary;
use crate::media::MediaSummary;
use crate::signal::{is_signal_export, is_signal_export_reader, SignalSource};
use crate::telegram::{is_telegram_export, is_telegram_export_reader, TelegramSource};
use crate::whatsapp::{is_whatsapp_export, is_whatsapp_export_reader, WhatsappSource};
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

/// [`detect_chat_format`] streamed from the picked file: detection reads
/// the format probes' bounded views (the JSON walkers skip `messages`
/// element by element; WhatsApp only looks at the first 30 lines), so an
/// arbitrarily large export is detected without loading it (M2.2).
pub fn detect_chat_format_reader(path: &Path) -> Result<Option<ChatFormat>> {
    let file = fs::File::open(path)?;
    let mut file = std::io::BufReader::new(file);
    use std::io::Seek as _;
    if is_discord_export_reader(&mut file) {
        return Ok(Some(ChatFormat::Discord));
    }
    file.rewind()?;
    if is_telegram_export_reader(&mut file) {
        return Ok(Some(ChatFormat::Telegram));
    }
    file.rewind()?;
    if is_signal_export_reader(&mut file) {
        return Ok(Some(ChatFormat::Signal));
    }
    file.rewind()?;
    if is_whatsapp_export_reader(&mut file) {
        return Ok(Some(ChatFormat::WhatsApp));
    }
    Ok(None)
}

/// Outcome of a chat export import: the ingest summary plus (when media
/// extraction ran) how the media pass went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImportOutcome {
    pub summary: IngestSummary,
    pub media: Option<MediaSummary>,
}

impl From<BoundedOutcome> for ChatImportOutcome {
    fn from(outcome: BoundedOutcome) -> Self {
        Self {
            summary: outcome.summary,
            media: outcome.media,
        }
    }
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

/// Resolve a picked path to its format and its **streaming** source (the
/// bounded counterpart of `import`'s detection step). A directory pick is
/// a Discord Data Package by this point (`resolve_export_file` returns
/// the dir itself only in that case).
pub fn resolve_stream_source(
    picked: &Path,
) -> Result<(ChatFormat, PathBuf, Box<dyn StreamSource + Send>)> {
    let file = resolve_export_file(picked)?;
    if file.is_dir() {
        let source = DiscordSource::new(file.clone());
        return Ok((ChatFormat::Discord, file, Box::new(source)));
    }
    let format = detect_chat_format_reader(&file)?.ok_or_else(|| {
        Error::ChatImport(format!(
            "unrecognized chat export format: {} (expected a WhatsApp .txt export, a Telegram result.json, a Signal conversation .json, or a Discord messages.json)",
            file.display()
        ))
    })?;
    let source: Box<dyn StreamSource + Send> = match format {
        ChatFormat::Telegram => Box::new(TelegramSource::new(&file)),
        ChatFormat::Signal => Box::new(SignalSource::new(&file)),
        ChatFormat::WhatsApp => Box::new(WhatsappSource::new(&file)),
        ChatFormat::Discord => Box::new(DiscordSource::new(&file)),
    };
    Ok((format, file, source))
}

/// The bounded production import (M2.2): same formats, same detection,
/// same media behavior as [`import`], but streamed — the parser never
/// holds the archive, batches commit atomically with checkpoint
/// advancement, cancellation preserves committed data and correct
/// counts, and an interrupted run resumes from its checkpoint on the
/// next call with the same input (see [`crate::bounded`]).
///
/// Media extraction runs per batch on the caller's thread (file copies
/// stay outside every storage lock); Signal and Discord record
/// attachments in `raw_json` only, as in [`import`].
pub fn import_bounded(
    sink: &mut dyn IngestSink,
    picked: &Path,
    extract_media: bool,
    data_dir: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64, &str),
) -> Result<BoundedOutcome> {
    let (format, file, source) = resolve_stream_source(picked)?;
    let mut run = BoundedRun::new(import_id_for("chat-import", &file));
    let export_root = file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if extract_media {
        if let Some(data_dir) = data_dir {
            match format {
                ChatFormat::Telegram => {
                    run.media = Some((MediaKind::Telegram, export_root.as_path(), data_dir))
                }
                ChatFormat::WhatsApp => {
                    run.media = Some((MediaKind::WhatsApp, export_root.as_path(), data_dir))
                }
                // Signal and Discord attachments stay recorded in `raw_json`
                // only — media extraction is not implemented for them yet.
                ChatFormat::Signal | ChatFormat::Discord => {}
            }
        }
    }
    run.run(source.as_ref(), sink, is_cancelled, progress)
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
