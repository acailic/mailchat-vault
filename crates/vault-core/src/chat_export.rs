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
    import_id_for, BoundedOutcome, BoundedRun, IngestSink, MediaKind, MediaPass, StreamSource,
};
use crate::discord::{
    is_discord_export, is_discord_export_dir, is_discord_export_reader, DiscordSource,
};
use crate::import_bounds::{expand_zip_bounded, ImportBounds};
use crate::ingest::IngestSummary;
use crate::jsonl::{is_jsonl_export, is_jsonl_export_reader, JsonlSource};
use crate::media::MediaSummary;
use crate::messenger::{
    is_messenger_export, is_messenger_export_dir, is_messenger_export_reader,
    messenger_export_root, MessengerSource,
};
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
    Messenger,
    /// The app's own JSON Lines export (`.jsonl`): one stored message
    /// object per line, each row keeping its original source identity.
    Jsonl,
}

/// Detect which chat export format `bytes` hold, if any.
///
/// Discord wins when the JSON carries a `messages` array plus a
/// `channel`/`guild` object (Telegram exports also have a top-level
/// `messages` array, so the Discord shapes must be checked first);
/// a Messenger export (a `participants` array plus a `messages` array
/// whose first element carries `sender_name`/`timestamp_ms`) is
/// checked next, before the Telegram probe can claim it; a top-level
/// JSON array of Signal Desktop message objects is a Signal
/// conversation export; otherwise a Telegram `messages` array
/// decides, then a WhatsApp header within the first lines; last, a
/// first line shaped like the app's own JSON Lines export rows (an
/// object with `source` + `external_id`) claims it. Anything
/// else returns `None`.
pub fn detect_chat_format(bytes: &[u8]) -> Option<ChatFormat> {
    if is_discord_export(bytes) {
        return Some(ChatFormat::Discord);
    }
    if is_messenger_export(bytes) {
        return Some(ChatFormat::Messenger);
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
    if is_jsonl_export(bytes) {
        return Some(ChatFormat::Jsonl);
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
    if is_messenger_export_reader(&mut file) {
        return Ok(Some(ChatFormat::Messenger));
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
    file.rewind()?;
    if is_jsonl_export_reader(&mut file) {
        return Ok(Some(ChatFormat::Jsonl));
    }
    Ok(None)
}

/// Outcome of a chat export import: the ingest summary, the bounded
/// run's import id (the "View imported messages" scope), and (when media
/// extraction ran) how the media pass went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImportOutcome {
    pub summary: IngestSummary,
    /// The import run's id (an import-scope filter value).
    #[serde(default)]
    pub import_id: String,
    pub media: Option<MediaSummary>,
}

impl From<BoundedOutcome> for ChatImportOutcome {
    fn from(outcome: BoundedOutcome) -> Self {
        Self {
            summary: outcome.summary,
            import_id: outcome.import_id,
            media: outcome.media,
        }
    }
}

/// Resolve the picked path to the export file: `result.json` inside a
/// picked directory, the picked directory itself when it is a Discord
/// Data Package or a Messenger DYI tree (the export root, `messages/`,
/// `messages/inbox/`, or a thread folder — its `message_*.json` parts
/// merge into one thread), or the picked file itself. A directory whose
/// immediate entries hold no export may descend a single subdirectory
/// chain (exports are commonly wrapped — `ChatExport…/result.json`
/// inside a Telegram ZIP), and a directory holding exactly one
/// recognizable export file next to media folders resolves to that file
/// (a WhatsApp ZIP: the `.txt` plus `photos/`…). Hidden entries
/// (`.`-prefixed, `__MACOSX/`, AppleDouble `._*`) never count.
fn resolve_export_file(picked: &Path) -> Result<PathBuf> {
    if picked.is_dir() {
        let mut dir = picked.to_path_buf();
        // Wrapped exports nest a couple of levels at most; a hard cap
        // keeps even a pathological self-referencing tree finite.
        for _ in 0..16 {
            let result_json = dir.join("result.json");
            if result_json.is_file() {
                return Ok(result_json);
            }
            if is_discord_export_dir(&dir) {
                return Ok(dir);
            }
            if is_messenger_export_dir(&dir) {
                return Ok(dir);
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                return Err(Error::ChatImport(format!(
                    "unreadable chat export directory: {}",
                    dir.display()
                )));
            };
            let mut subdirs = Vec::new();
            let mut export_files: Vec<(PathBuf, ChatFormat)> = Vec::new();
            let mut other_files = 0usize;
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || name == "__MACOSX" || name.starts_with("._") {
                    continue; // hidden / macOS archive junk
                }
                if file_type.is_dir() {
                    subdirs.push(entry.path());
                } else if file_type.is_file() {
                    match detect_chat_format_reader(&entry.path()) {
                        Ok(Some(format)) => export_files.push((entry.path(), format)),
                        _ => other_files += 1,
                    }
                }
            }
            // A single detectable export file wins (its sibling folders
            // are media, not competing exports).
            if export_files.len() == 1 {
                return Ok(export_files.remove(0).0);
            }
            // Several Messenger message files are one thread's parts —
            // the pick is the thread folder itself.
            if !export_files.is_empty()
                && export_files
                    .iter()
                    .all(|(_, f)| *f == ChatFormat::Messenger)
            {
                return Ok(dir);
            }
            if export_files.len() > 1 {
                return Err(Error::ChatImport(format!(
                    "directory holds {} export files; pick one file explicitly",
                    export_files.len()
                )));
            }
            // No export file here: descend when exactly one plain
            // subdirectory exists (the wrapped-export shape).
            if subdirs.len() == 1 && other_files == 0 {
                dir = subdirs.remove(0);
                continue;
            }
            return Err(Error::ChatImport(format!(
                "unrecognized chat export format: {} (expected a directory containing a Telegram result.json, a Discord Data Package, a Messenger message_*.json, or a single export file)",
                picked.display()
            )));
        }
        return Err(Error::ChatImport(format!(
            "unrecognized chat export format: {} (directory nests too deeply)",
            picked.display()
        )));
    }
    Ok(picked.to_path_buf())
}

/// Resolve a picked path to its format and its **streaming** source (the
/// bounded counterpart of `import`'s detection step). A directory pick is
/// a Discord Data Package or a Messenger DYI tree by this point
/// (`resolve_export_file` returns the dir itself only in those cases).
pub fn resolve_stream_source(
    picked: &Path,
) -> Result<(ChatFormat, PathBuf, Box<dyn StreamSource + Send>)> {
    let file = resolve_export_file(picked)?;
    if file.is_dir() {
        if is_discord_export_dir(&file) {
            let source = DiscordSource::new(file.clone());
            return Ok((ChatFormat::Discord, file, Box::new(source)));
        }
        let source = MessengerSource::new(file.clone());
        return Ok((ChatFormat::Messenger, file, Box::new(source)));
    }
    let format = detect_chat_format_reader(&file)?.ok_or_else(|| {
        Error::ChatImport(format!(
            "unrecognized chat export format: {} (expected a WhatsApp .txt export, a Telegram result.json, a Messenger message_*.json, a Signal conversation .json, a Discord messages.json, or a JSON Lines .jsonl export)",
            file.display()
        ))
    })?;
    let source: Box<dyn StreamSource + Send> = match format {
        ChatFormat::Telegram => Box::new(TelegramSource::new(&file)),
        ChatFormat::Signal => Box::new(SignalSource::new(&file)),
        ChatFormat::WhatsApp => Box::new(WhatsappSource::new(&file)),
        ChatFormat::Discord => Box::new(DiscordSource::new(&file)),
        ChatFormat::Messenger => Box::new(MessengerSource::new(&file)),
        ChatFormat::Jsonl => Box::new(JsonlSource::new(&file)),
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
/// attachments in `raw_json` only, as in [`import`], and JSON Lines
/// rows keep their `raw_json` media references as references — media
/// bytes are not transferred. A Messenger tree
/// pick (directory or `.zip`) configures its media pass at the DYI
/// export root, so message-referenced files resolve, store, and link
/// from `raw_json`; a single-file Messenger pick says nothing about
/// the tree around it, so its references stay in `raw_json` only.
///
/// A picked `.zip` (M4.1) is expanded first — through the shared
/// traversal/symlink/capacity fence ([`expand_zip_bounded`]) into an
/// OS-temp directory that lives exactly as long as the import — and the
/// extracted tree is then resolved like a directory pick. A malicious
/// or oversized archive is refused before any message is stored.
pub fn import_bounded(
    sink: &mut dyn IngestSink,
    picked: &Path,
    extract_media: bool,
    data_dir: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64, &str),
) -> Result<BoundedOutcome> {
    // A ZIP pick expands to a temp tree; the guard keeps it alive for
    // the whole run (batches read media files out of it) and cleans it
    // up — including on error — when the run ends.
    let _zip_guard;
    let effective: std::borrow::Cow<'_, Path> = if picked.is_file()
        && picked
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        let extracted = tempfile::tempdir().map_err(Error::from)?;
        expand_zip_bounded(picked, extracted.path(), &ImportBounds::default())?;
        let root = extracted.path().to_path_buf();
        _zip_guard = extracted;
        std::borrow::Cow::Owned(root)
    } else {
        std::borrow::Cow::Borrowed(picked)
    };
    // A whole-tree pick (a directory, or a `.zip` that expands into
    // one) asserts that everything inside is export content: unlinked
    // files — files no message references — are stored too (M4.2). A
    // bare export file says nothing about its parent directory, so its
    // import never reaches beyond the referenced files themselves.
    let tree_pick = picked.is_dir()
        || (picked.is_file()
            && picked
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("zip")));
    let (format, file, source) = resolve_stream_source(&effective)?;
    // Media-pass path comparisons (consumed paths, the walked tree) are
    // canonical; the export file itself must be in that same form.
    let export_file = file.canonicalize()?;
    // The DYI root a Messenger tree pick resolves media against (its
    // uris are `messages/…`-rooted); the other kinds never read it.
    let messenger_root = messenger_export_root(&file);
    let mut run = BoundedRun::new(import_id_for("chat-import", &file));
    let export_root = file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if extract_media {
        if let Some(data_dir) = data_dir {
            // Signal and Discord attachments stay recorded in `raw_json`
            // only — media extraction is not implemented for them yet.
            // JSON Lines rows keep their `raw_json` media references as
            // references (media bytes are not transferred; the backup
            // container is the whole-vault tool). Messenger joins the
            // media kinds only for a tree pick: a single-file pick says
            // nothing about the tree around it, so its references stay
            // in `raw_json` only (spec decision).
            let kind = match format {
                ChatFormat::Telegram => Some(MediaKind::Telegram),
                ChatFormat::WhatsApp => Some(MediaKind::WhatsApp),
                ChatFormat::Messenger if file.is_dir() => Some(MediaKind::Messenger),
                ChatFormat::Signal
                | ChatFormat::Discord
                | ChatFormat::Messenger
                | ChatFormat::Jsonl => None,
            };
            if let Some(kind) = kind {
                let pass = if kind == MediaKind::Messenger {
                    MediaPass {
                        kind,
                        export_root: messenger_root.as_path(),
                        export_file: None,
                        data_dir,
                        // Only a pick of the DYI root itself sweeps
                        // unreferenced files — a thread-folder or
                        // `messages/` pick must not swallow the tree
                        // around it. Both sides derive from the same
                        // pick, so the plain comparison is exact.
                        unlinked: tree_pick && messenger_root.as_path() == effective.as_ref(),
                    }
                } else {
                    MediaPass {
                        kind,
                        export_root: export_root.as_path(),
                        export_file: Some(export_file.as_path()),
                        data_dir,
                        unlinked: tree_pick,
                    }
                };
                run.media = Some(pass);
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
    fn messenger_thread_json_is_not_telegram() {
        // The Telegram probe is "has a messages array"; Messenger must be
        // checked first or a DYI file would be imported as Telegram.
        let messenger = br#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "hi"}]}"#;
        assert_eq!(detect_chat_format(messenger), Some(ChatFormat::Messenger));
    }

    #[test]
    fn reader_detection_pins_the_same_order() {
        // The reader variant real picks use must answer in the same
        // Discord → Messenger → Telegram order as the bytes probe:
        // a Discord temp file answers Discord, a Messenger one
        // Messenger — never Telegram.
        let dir = tempfile::tempdir().unwrap();
        let discord = dir.path().join("messages.json");
        std::fs::write(&discord, discord_bytes()).unwrap();
        assert_eq!(
            detect_chat_format_reader(&discord).unwrap(),
            Some(ChatFormat::Discord)
        );
        let messenger = dir.path().join("message_1.json");
        std::fs::write(
            &messenger,
            br#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "hi"}]}"#,
        )
        .unwrap();
        assert_eq!(
            detect_chat_format_reader(&messenger).unwrap(),
            Some(ChatFormat::Messenger)
        );
        let telegram = dir.path().join("result.json");
        std::fs::write(&telegram, telegram_bytes()).unwrap();
        assert_eq!(
            detect_chat_format_reader(&telegram).unwrap(),
            Some(ChatFormat::Telegram)
        );
    }

    #[test]
    fn messenger_dir_resolves_over_the_export_files_error() {
        // A thread folder holds multiple message_*.json files; as generic
        // export files that errors ("pick one file") — for Messenger it
        // is one thread pick.
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(
            thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "x"}]}"#,
        )
        .unwrap();
        std::fs::write(
            thread.join("message_2.json"),
            r#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 2, "content": "y"}]}"#,
        )
        .unwrap();
        let (format, file, _source) = resolve_stream_source(&thread).unwrap();
        assert_eq!(format, ChatFormat::Messenger);
        assert!(file.is_dir());
    }

    #[test]
    fn renamed_messenger_parts_resolve_to_their_folder() {
        // Parts that lost their `message_N` names are still recognized
        // by content: an all-Messenger set resolves to the folder, not
        // to the "pick one file" error.
        let dir = tempfile::tempdir().unwrap();
        for name in ["prvi.json", "drugi.json"] {
            std::fs::write(
                dir.path().join(name),
                r#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "x"}]}"#,
            )
            .unwrap();
        }
        assert_eq!(resolve_export_file(dir.path()).unwrap(), dir.path());
    }

    #[test]
    fn mixed_format_directory_still_asks_for_one_file() {
        // The genuine multi-format case keeps its error text (a Discord
        // json does not qualify — any dir holding one is a Discord pick).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chat.txt"), WHATSAPP_BYTES).unwrap();
        std::fs::write(dir.path().join("result_telegram.json"), telegram_bytes()).unwrap();
        let err = resolve_export_file(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("directory holds 2 export files; pick one file explicitly"),
            "{err}"
        );
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
