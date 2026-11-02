//! Chat export format detection and import for the generic "import chat
//! export" path: the picked file's contents decide which source parses it.
//!
//! [`import`] orchestrates the whole flow — detect, collect, extract
//! media (when enabled and a data dir is given), then ingest with media
//! backfill — so the Tauri shell stays thin and the flow is testable
//! without the app.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
}

/// Detect which chat export format `bytes` hold, if any.
///
/// Telegram wins when the JSON parses into an export with a `messages`
/// array; a top-level JSON array of Signal Desktop message objects is a
/// Signal conversation export; otherwise a WhatsApp header within the
/// first lines decides. Anything else returns `None`.
pub fn detect_chat_format(bytes: &[u8]) -> Option<ChatFormat> {
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
/// `result.json` (a directory containing `result.json` works too), or a
/// Signal conversation `.json` (e.g. from `sigtop export-messages -f
/// json`, one file per conversation). The format is detected from the
/// file contents.
///
/// Media extraction runs when `extract_media` is set and `data_dir` (the
/// app data dir that will hold the `media/` store) is given: export files
/// are copied into `data_dir/media/` hash-named and linked from each
/// message's `raw_json`; messages already in the archive get their links
/// backfilled, so re-importing an export with media enriches it. Pass
/// `extract_media = false` (or `data_dir = None`) to import text only.
/// Signal exports record attachments in `raw_json` only — media
/// extraction is not implemented for Signal yet.
pub fn import(
    storage: &Storage,
    picked: &Path,
    extract_media: bool,
    data_dir: Option<&Path>,
) -> Result<ChatImportOutcome> {
    let file = resolve_export_file(picked)?;
    let bytes = std::fs::read(&file)?;
    let format = detect_chat_format(&bytes).ok_or_else(|| {
        Error::ChatImport(format!(
            "unrecognized chat export format: {} (expected a WhatsApp .txt export, a Telegram result.json, or a Signal conversation .json)",
            file.display()
        ))
    })?;

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
                // Signal attachments stay recorded in `raw_json` only —
                // media extraction is not implemented for Signal yet.
                ChatFormat::Signal => {}
            }
        }
    }

    let source_name = match format {
        ChatFormat::Telegram => crate::telegram::TELEGRAM_SOURCE,
        ChatFormat::Signal => SIGNAL_SOURCE,
        ChatFormat::WhatsApp => crate::whatsapp::WHATSAPP_SOURCE,
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
/// picked directory, or the picked file itself.
fn resolve_export_file(picked: &Path) -> Result<PathBuf> {
    if picked.is_dir() {
        let result_json = picked.join("result.json");
        if !result_json.is_file() {
            return Err(Error::ChatImport(format!(
                "no result.json found in {}",
                picked.display()
            )));
        }
        Ok(result_json)
    } else {
        Ok(picked.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHATSAPP_BYTES: &[u8] = b"[1.11.2026., 09:00:05] Marko: Zdravo ekipa!\n";

    fn telegram_bytes() -> Vec<u8> {
        br#"{"name": "Arhiva", "type": "private_group", "id": 42, "messages": [{"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "from": "Marko", "text": "hi"}]}"#
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
