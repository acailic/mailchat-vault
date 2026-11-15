//! Bounded import preview (M3.3, the first-source journey): look at a
//! picked file, directory, or `.zip` **before** anything is stored.
//!
//! The preview answers the questions a first import should answer up
//! front — what did I pick, what format is it, roughly how much is in
//! it, does it reference media files that are (or are not) present —
//! without committing a single row. It reuses the exact detection and
//! streaming surfaces the import itself uses
//! ([`crate::mbox::MboxSource`] and [`crate::chat_export`]), so what the
//! preview says is what the import will find.
//!
//! Everything here is bounded by design: the item scan stops at
//! [`SCAN_ITEM_CAP`] entries (counts then read as lower bounds with
//! `scan_complete = false`, never as exact totals), sample bodies are
//! truncated, and the tree walk for input size goes through the M4.1
//! import bounds. Nothing is written to any database or file.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bounded::{Flow, StreamItem, StreamSource};
use crate::chat_export::{self, ChatFormat};
use crate::import_bounds::{expand_zip_bounded, walk_bounded, ImportBounds};
use crate::mbox::{EmlDirSource, MboxSource, MBOX_SOURCE};
use crate::messenger::messenger_export_root;
use crate::{Error, Result};

/// How many items (messages + parse failures) one preview scans before
/// stopping: enough to know the format parses and to sample it, cheap
/// enough to feel instant on any machine.
pub const SCAN_ITEM_CAP: u64 = 500;

/// How many sample messages the preview shows.
pub const SAMPLE_CAP: usize = 3;

/// Snippet length for samples (characters).
const SNIPPET_CHARS: usize = 160;

/// One sample message shown in the preview (all strings bounded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreviewSample {
    /// Display author ("" = unknown).
    pub author: String,
    /// Subject or conversation label when the format carries one.
    pub subject: String,
    /// RFC 3339 UTC instant; `None` when the message carries no usable
    /// timestamp (the preview shows "no date" rather than guessing).
    pub sent_at: Option<String>,
    /// First `SNIPPET_CHARS` characters of the body.
    pub snippet: String,
}

/// Bounded preview of one picked import input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreview {
    /// Machine-readable format id: `mbox`, `eml`, `whatsapp`,
    /// `telegram`, `signal`, `discord`, `messenger`, or `jsonl`.
    pub kind: String,
    /// The dedup namespace the import will store under (`source.name()`).
    pub source: String,
    /// Total size of the picked input in bytes (the file, or the bounded
    /// walk total for a directory/`.zip` tree pick).
    pub input_bytes: u64,
    /// Messages seen within the bounded scan; a lower bound when
    /// `scan_complete` is `false`.
    pub messages_seen: u64,
    /// Whether the scan reached the end of the input (`false` = every
    /// count here is a lower bound, not an exact total).
    pub scan_complete: bool,
    /// Entries the parsers could not turn into messages within the scan.
    pub parse_failures: u64,
    /// First [`SAMPLE_CAP`] sample messages.
    pub samples: Vec<ImportPreviewSample>,
    /// Scanned messages that reference at least one media file.
    pub media_referenced: u64,
    /// Referenced media files provably absent from the export (Telegram
    /// and Messenger tree references resolve exactly, so absence is
    /// certain; WhatsApp name matching is fuzzier, so absence is not
    /// claimed there).
    pub media_missing: u64,
    /// Human-readable caveats ("scan stopped at 500 items…").
    pub warnings: Vec<String>,
}

/// A previewed input, split into what to render and what to import.
struct PreviewSource {
    kind: String,
    source: String,
    /// The streaming source to scan.
    stream: Box<dyn StreamSource + Send>,
    /// Root media references resolve against (None for mbox/eml, and for
    /// a lone Messenger file, which says nothing about the tree around
    /// it — the import extracts no media for that pick either).
    export_root: Option<PathBuf>,
    /// Whether the input is a whole tree (directory or expanded `.zip`)
    /// whose size is measured by a bounded walk instead of one file.
    tree_input: bool,
    /// What the bounded size walk measures: the picked path itself, or
    /// the extracted tree root for a `.zip` pick.
    size_root: PathBuf,
}

/// Does the file start with an mbox `From ` separator line? (Bounded:
/// only the first line is read.)
fn looks_like_mbox(path: &Path) -> Result<bool> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    Ok(first.starts_with("From "))
}

/// Does the file start like a bare mail (an RFC 5322 header line —
/// `Name: value`)? Only consulted when the mbox `From ` sniff failed,
/// and only when the field name leads with a letter — chat exports
/// must stay on their own path (JSON leads with `{`, a WhatsApp `.txt`
/// with `[date`, whose name also holds a space). (Bounded: one line.)
fn looks_like_eml(path: &Path) -> Result<bool> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let Some((name, _rest)) = first.split_once(':') else {
        return Ok(false);
    };
    Ok(name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && name.bytes().all(|b| (33..=126).contains(&b) && b != b':'))
}

fn format_name(format: ChatFormat) -> &'static str {
    match format {
        ChatFormat::WhatsApp => "whatsapp",
        ChatFormat::Telegram => "telegram",
        ChatFormat::Signal => "signal",
        ChatFormat::Discord => "discord",
        ChatFormat::Messenger => "messenger",
        ChatFormat::Jsonl => "jsonl",
    }
}

/// Resolve a chat-export source from a file path (bare pick).
fn resolve_chat_file(file: &Path) -> Result<PreviewSource> {
    let (format, file, stream) = chat_export::resolve_stream_source(file)?;
    let export_root = if format == ChatFormat::Messenger {
        // A lone Messenger file says nothing about the tree around it —
        // the import runs no media pass for it either, so the preview
        // counts references without ever claiming absence.
        None
    } else {
        Some(
            file.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        )
    };
    Ok(PreviewSource {
        kind: format_name(format).into(),
        source: stream.name().to_string(),
        stream,
        export_root,
        tree_input: false,
        size_root: file.to_path_buf(),
    })
}

/// Resolve a directory pick (or an extracted `.zip` root) to a chat
/// export source — or, when the chat side does not recognize the
/// folder but it holds `.eml` files, to an Outlook drag-out folder
/// (one mail per file). `display` is only used in error messages.
fn resolve_dir_source(root: &Path, display: &Path) -> Result<PreviewSource> {
    match chat_export::resolve_stream_source(root) {
        Ok((format, file, stream)) => {
            let export_root = if format == ChatFormat::Messenger {
                // DYI uris are `messages/…`-rooted: they resolve at the
                // export root whatever tree level was picked, the same
                // root the import's media pass resolves against
                // ([`messenger_export_root`]).
                Some(messenger_export_root(&file))
            } else {
                Some(
                    file.parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| root.to_path_buf()),
                )
            };
            Ok(PreviewSource {
                kind: format_name(format).into(),
                source: stream.name().to_string(),
                stream,
                export_root,
                tree_input: true,
                size_root: root.to_path_buf(),
            })
        }
        Err(e) => {
            // The same wrapped-folder descent the mail import pick
            // applies, so the preview and the import agree on the
            // "compress folder" zip shape (a lone subdirectory holding
            // the `.eml` files).
            let eml_root = crate::mbox::descend_wrapped_eml_dir(root.to_path_buf());
            let has_eml = walk_eml_count(&eml_root);
            if has_eml > 0 {
                return Ok(PreviewSource {
                    kind: "eml".into(),
                    source: MBOX_SOURCE.to_string(),
                    stream: Box::new(EmlDirSource::new(eml_root)),
                    export_root: None,
                    tree_input: true,
                    size_root: root.to_path_buf(),
                });
            }
            Err(Error::ChatImport(format!(
                "unrecognized export in {} — expected a Telegram result.json export folder, \
                 a Discord Data Package, or a folder holding one chat export file ({e})",
                display.display()
            )))
        }
    }
}

/// `.eml` files directly inside the directory (bounded: one read_dir).
fn walk_eml_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.path().is_file()
                        && e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| x.eq_ignore_ascii_case("eml"))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Resolve a picked path to the source that would import it, or a
/// friendly error when nothing recognizes the input. A `.zip` pick is
/// expanded through the same M4.1 fence the import uses; the returned
/// guard keeps the extracted tree alive for the scan.
fn resolve_preview_source(picked: &Path) -> Result<(PreviewSource, Option<tempfile::TempDir>)> {
    let missing = |p: &Path| {
        Error::ChatImport(format!(
            "cannot read {}: file or folder not found — pick it again",
            p.display()
        ))
    };
    if picked.is_file() {
        if picked
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            let extracted = tempfile::tempdir().map_err(Error::from)?;
            expand_zip_bounded(picked, extracted.path(), &ImportBounds::default())?;
            let mut source = resolve_dir_source(extracted.path(), picked)?;
            source.size_root = extracted.path().to_path_buf();
            return Ok((source, Some(extracted)));
        }
        let is_mbox = picked
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mbox"))
            || looks_like_mbox(picked)?;
        if is_mbox {
            return Ok((
                PreviewSource {
                    kind: "mbox".into(),
                    source: MBOX_SOURCE.to_string(),
                    stream: Box::new(MboxSource::new(picked)),
                    export_root: None,
                    tree_input: false,
                    size_root: picked.to_path_buf(),
                },
                None,
            ));
        }
        let is_eml = picked
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("eml"))
            || looks_like_eml(picked)?;
        if is_eml {
            return Ok((
                PreviewSource {
                    kind: "eml".into(),
                    source: MBOX_SOURCE.to_string(),
                    stream: Box::new(MboxSource::new(picked)),
                    export_root: None,
                    tree_input: false,
                    size_root: picked.to_path_buf(),
                },
                None,
            ));
        }
        return Ok((resolve_chat_file(picked)?, None));
    }
    if picked.is_dir() {
        return Ok((resolve_dir_source(picked, picked)?, None));
    }
    Err(missing(picked))
}

/// The explicit media references of one Telegram message object
/// (`photo`/`file` path strings, or the newer nested-object shape).
fn telegram_ref_paths(obj: &Map<String, Value>) -> Vec<&str> {
    let mut refs = Vec::new();
    for field in ["photo", "file"] {
        match obj.get(field) {
            Some(Value::String(rel)) if !rel.is_empty() => refs.push(rel.as_str()),
            Some(Value::Object(inner)) => {
                for nested in ["file", "photo"] {
                    if let Some(Value::String(rel)) = inner.get(nested) {
                        if !rel.is_empty() {
                            refs.push(rel.as_str());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    refs
}

/// The explicit media references of one Messenger (DYI) message object:
/// the `uri` of each item in the five media arrays plus the single
/// `sticker` object — the same fields [`crate::media::messenger_refs`]
/// reads — every uri a path relative to the DYI export root.
fn messenger_ref_paths(obj: &Map<String, Value>) -> Vec<&str> {
    let mut refs = Vec::new();
    for field in ["photos", "videos", "audio_files", "files", "gifs"] {
        if let Some(Value::Array(items)) = obj.get(field) {
            for item in items {
                if let Some(uri) = item.get("uri").and_then(Value::as_str) {
                    if !uri.is_empty() {
                        refs.push(uri);
                    }
                }
            }
        }
    }
    if let Some(uri) = obj
        .get("sticker")
        .and_then(Value::as_object)
        .and_then(|s| s.get("uri"))
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
    {
        refs.push(uri);
    }
    refs
}

/// Media presence of one scanned message: `(references the message
/// carries, files provably absent from the export)`.
///
/// Telegram and Messenger references resolve exactly under the export
/// root, so absence is provable; WhatsApp and Signal attachment names
/// match fuzzily at import time (recursive walk), so a reference is
/// reported but absence is never claimed.
fn media_refs_of(kind: &str, raw_json: &str, export_root: Option<&Path>) -> (u64, u64) {
    let Ok(value) = serde_json::from_str::<Value>(raw_json) else {
        return (0, 0);
    };
    let Some(obj) = value.as_object() else {
        return (0, 0);
    };
    // The exact-resolution kinds, selected by the preview's format: each
    // counts only its own reference shapes.
    let exact: Vec<&str> = match kind {
        "telegram" => telegram_ref_paths(obj),
        "messenger" => messenger_ref_paths(obj),
        _ => Vec::new(),
    };
    if !exact.is_empty() {
        let missing = export_root
            .map(|root| {
                exact
                    .iter()
                    .filter(|rel| {
                        crate::import_bounds::safe_child(root, rel).map(|p| p.is_file())
                            != Some(true)
                    })
                    .count() as u64
            })
            .unwrap_or(0);
        return (1, missing);
    }
    let whatsapp = kind == "whatsapp"
        && obj
            .get("attachment")
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|n| !n.is_empty());
    if whatsapp {
        return (1, 0);
    }
    // Signal attachments: a named `attachments` entry is a reference
    // (the media pass matches it fuzzily by name, so absence is never
    // claimed); name-less entries (voice notes) resolve nothing.
    let signal = kind == "signal"
        && obj
            .get("attachments")
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry
                        .get("fileName")
                        .and_then(Value::as_str)
                        .is_some_and(|n| !n.is_empty())
                })
            });
    if signal {
        return (1, 0);
    }
    // JSON Lines rows carry their media as already-stored `media`
    // entries in `raw_json` (digest references into the exporting
    // vault's object store): counted as references, but the bytes are
    // not transferred, so absence is never claimed.
    if kind == "jsonl"
        && obj
            .get("media")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        return (1, 0);
    }
    (0, 0)
}

/// Preview a picked import input: detect the format, scan up to
/// [`SCAN_ITEM_CAP`] items, and report bounded counts, samples, and
/// media presence — all without storing anything.
pub fn preview_import(picked: &Path) -> Result<ImportPreview> {
    let (resolved, _zip_guard) = resolve_preview_source(picked)?;
    let mut messages_seen: u64 = 0;
    let mut parse_failures: u64 = 0;
    let mut media_referenced: u64 = 0;
    let mut media_missing: u64 = 0;
    let mut samples: Vec<ImportPreviewSample> = Vec::new();
    let mut scan_complete = true;
    resolved
        .stream
        .stream(0, &mut |item: StreamItem| -> Result<Flow> {
            match item {
                StreamItem::Message(msg) => {
                    messages_seen += 1;
                    let (refs, missing) = media_refs_of(
                        &resolved.kind,
                        &msg.raw_json,
                        resolved.export_root.as_deref(),
                    );
                    media_referenced += refs;
                    media_missing += missing;
                    if samples.len() < SAMPLE_CAP {
                        samples.push(ImportPreviewSample {
                            author: msg.author,
                            subject: msg
                                .body
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(80)
                                .collect(),
                            sent_at: if msg.sent_at.is_empty() {
                                None
                            } else {
                                Some(msg.sent_at)
                            },
                            snippet: msg.body.chars().take(SNIPPET_CHARS).collect(),
                        });
                    }
                }
                StreamItem::Failed { .. } => {
                    parse_failures += 1;
                }
            }
            if messages_seen + parse_failures >= SCAN_ITEM_CAP {
                scan_complete = false;
                Ok(Flow::Stop)
            } else {
                Ok(Flow::Continue)
            }
        })?;
    let mut warnings = Vec::new();
    if !scan_complete {
        warnings.push(format!(
            "the preview scanned the first {SCAN_ITEM_CAP} items — every count is a lower bound, not an exact total"
        ));
    }
    if parse_failures > 0 {
        warnings.push(format!(
            "{parse_failures} scanned entries could not be parsed — the import records them as issues and continues"
        ));
    }
    // Input size: a bare file's own size, or the bounded walk total of a
    // tree input. A tree that crosses the import bounds fails the import
    // itself, so the same failure surfaces here as a warning.
    let input_bytes = if resolved.tree_input {
        match walk_bounded(&resolved.size_root, &ImportBounds::default()) {
            Ok(files) => files.iter().map(|f| f.size).sum(),
            Err(e) => {
                warnings.push(format!("could not measure the picked folder's size: {e}"));
                0
            }
        }
    } else {
        fs::metadata(picked).map(|m| m.len()).unwrap_or(0)
    };
    Ok(ImportPreview {
        kind: resolved.kind,
        source: resolved.source,
        input_bytes,
        messages_seen,
        scan_complete,
        parse_failures,
        samples,
        media_referenced,
        media_missing,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn previews_mbox() {
        let dir = tempdir();
        let path = dir.path().join("archive.mbox");
        write(
            &path,
            "From a@example.com Sun Nov  1 10:00:00 2026\n\
             From: A <a@example.com>\nTo: b@example.com\nSubject: Hello\nDate: Sun, 1 Nov 2026 10:00:00 +0000\n\
             Message-ID: <m1@example.com>\n\nBody one.\n\
             From c@example.com Sun Nov  1 11:00:00 2026\n\
             From: C <c@example.com>\nSubject: Second\nDate: Sun, 1 Nov 2026 11:00:00 +0000\n\
             Message-ID: <m2@example.com>\n\nBody two.\n",
        );
        let preview = preview_import(&path).expect("mbox preview");
        assert_eq!(preview.kind, "mbox");
        assert_eq!(preview.source, "mbox");
        assert_eq!(preview.messages_seen, 2);
        assert!(preview.scan_complete);
        assert_eq!(preview.parse_failures, 0);
        assert_eq!(preview.samples.len(), 2);
        assert_eq!(preview.input_bytes, std::fs::metadata(&path).unwrap().len());
        assert!(preview.warnings.is_empty());
    }

    #[test]
    fn previews_eml_file() {
        let dir = tempdir();
        let path = dir.path().join("mail.eml");
        write(
            &path,
            "From: a@b.c\nSubject: eml prev\nMessage-ID: <p@x>\n\nPozdrav\n",
        );
        let preview = preview_import(&path).expect("eml preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.source, "mbox");
        assert_eq!(preview.messages_seen, 1);
    }

    #[test]
    fn renamed_eml_file_is_detected_by_content() {
        let dir = tempdir();
        let path = dir.path().join("mail.txt");
        write(
            &path,
            "Subject: hello\nFrom: a@b.c\nMessage-ID: <s@x>\n\nSniffed body\n",
        );
        let preview = preview_import(&path).expect("sniffed eml preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.messages_seen, 1);
    }

    #[test]
    fn previews_eml_directory() {
        let dir = tempdir();
        write(
            &dir.path().join("1.eml"),
            "Subject: a\nMessage-ID: <1@x>\n\nA\n",
        );
        write(
            &dir.path().join("2.eml"),
            "Subject: b\nMessage-ID: <2@x>\n\nB\n",
        );
        let preview = preview_import(dir.path()).expect("eml dir preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.messages_seen, 2);
    }

    #[test]
    fn zip_of_eml_files_previews_as_eml() {
        let dir = tempdir();
        let zip_path = dir.path().join("emls.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in [
            (
                "1.eml",
                b"Subject: a\nMessage-ID: <1@x>\n\nA\n".as_slice().to_vec(),
            ),
            (
                "2.eml",
                b"Subject: b\nMessage-ID: <2@x>\n\nB\n".as_slice().to_vec(),
            ),
        ] {
            zip.start_file(name, zip::write::FileOptions::<()>::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let preview = preview_import(&zip_path).expect("eml zip preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.messages_seen, 2);
    }

    #[test]
    fn wrapped_zip_of_eml_folder_previews_as_eml() {
        // The default "right-click folder → compress" shape: the zip's
        // root holds one wrapper directory, the `.eml` files inside it.
        // The chat side rejects it; the eml fallback must descend the
        // wrapper exactly as the import pick does.
        let dir = tempdir();
        let zip_path = dir.path().join("mail.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in [
            (
                "mail/1.eml",
                b"Subject: a\nMessage-ID: <1@x>\n\nA\n".as_slice().to_vec(),
            ),
            (
                "mail/2.eml",
                b"Subject: b\nMessage-ID: <2@x>\n\nB\n".as_slice().to_vec(),
            ),
        ] {
            zip.start_file(name, zip::write::FileOptions::<()>::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let preview = preview_import(&zip_path).expect("wrapped eml zip preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.source, "mbox");
        assert_eq!(preview.messages_seen, 2);
        assert_eq!(preview.parse_failures, 0);
    }

    #[test]
    fn previews_telegram_json_with_missing_media() {
        let dir = tempdir();
        let path = dir.path().join("result.json");
        write(
            &path,
            r#"{"name":"Chat","id":7,"messages":[
                {"id":1,"type":"message","date":"2026-11-01T10:00:00","from":"Marko","text":"hi","photo":"photos/absent.jpg"},
                {"id":2,"type":"message","date":"2026-11-01T10:01:00","from":"Ana","text":"yo"}
            ]}"#,
        );
        let preview = preview_import(&path).expect("telegram preview");
        assert_eq!(preview.kind, "telegram");
        assert_eq!(preview.messages_seen, 2);
        assert_eq!(preview.media_referenced, 1);
        assert_eq!(
            preview.media_missing, 1,
            "photo file is absent next to the export"
        );
        assert_eq!(preview.samples.len(), 2);
    }

    #[test]
    fn previews_whatsapp_txt_media_reference_without_missing_claim() {
        let dir = tempdir();
        let path = dir.path().join("chat.txt");
        write(
            &path,
            "[1.11.2026., 09:00:05] Marko: check this\n[1.11.2026., 09:00:06] Marko: \u{200e}IMG-20261101-WA0001.jpg (file attached)\n",
        );
        let preview = preview_import(&path).expect("whatsapp preview");
        assert_eq!(preview.kind, "whatsapp");
        assert_eq!(preview.media_referenced, 1);
        assert_eq!(
            preview.media_missing, 0,
            "whatsapp name matching is fuzzy — no missing claim"
        );
    }

    #[test]
    fn previews_discord_package_directory() {
        let dir = tempdir();
        let messages = dir.path().join("messages");
        std::fs::create_dir_all(&messages).unwrap();
        write(
            &messages.join("123456.json"),
            r#"{"channel": {"id": "123456", "name": "general"}, "guild": {"id": "100", "name": "G"}, "messages":[
                {"id": "1", "type": "Default", "timestamp": "2026-11-01 10:00:00.123+00:00", "content": "hello", "author": {"id": "9", "name": "ana", "nickname": "Ana"}}
            ]}"#,
        );
        let preview = preview_import(&messages).expect("discord dir preview");
        assert_eq!(preview.kind, "discord");
        assert_eq!(preview.messages_seen, 1);
    }

    #[test]
    fn previews_messenger_tree() {
        let dir = tempdir();
        let thread = dir.path().join("messages/inbox/ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(
            thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
                "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "zdravo"}]}"#,
        )
        .unwrap();
        let preview = preview_import(dir.path()).expect("messenger preview");
        assert_eq!(preview.kind, "messenger");
        assert_eq!(preview.source, "messenger");
        assert_eq!(preview.messages_seen, 1);
    }

    #[test]
    fn previews_messenger_tree_media_refs() {
        let dir = tempdir();
        let thread = dir.path().join("messages/inbox/ana_1111");
        std::fs::create_dir_all(thread.join("photos")).unwrap();
        std::fs::write(thread.join("photos/there.jpg"), "jpegbytes").unwrap();
        write(
            &thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
                "messages": [
                    {"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "p",
                     "photos": [{"uri": "messages/inbox/ana_1111/photos/there.jpg"}]},
                    {"sender_name": "Ana", "timestamp_ms": 1762981260000, "content": "q",
                     "sticker": {"uri": "messages/inbox/ana_1111/stickers/gone.webp"}}
                ]}"#,
        );
        let preview = preview_import(dir.path()).expect("messenger media preview");
        assert_eq!(preview.kind, "messenger");
        assert_eq!(preview.messages_seen, 2);
        assert_eq!(preview.media_referenced, 2);
        assert_eq!(
            preview.media_missing, 1,
            "uris resolve at the DYI root: the sticker is absent, the photo is not"
        );
    }

    #[test]
    fn messenger_single_file_pick_claims_no_missing_media() {
        let dir = tempdir();
        let thread = dir.path().join("messages/inbox/ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        write(
            &thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
                "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "p",
                    "photos": [{"uri": "messages/inbox/ana_1111/photos/absent.jpg"}]}]}"#,
        );
        let preview =
            preview_import(&thread.join("message_1.json")).expect("messenger file preview");
        assert_eq!(preview.kind, "messenger");
        assert_eq!(preview.media_referenced, 1);
        assert_eq!(
            preview.media_missing, 0,
            "a lone file says nothing about the tree around it — no absence claim"
        );
    }

    #[test]
    fn scan_cap_makes_counts_lower_bounds() {
        let dir = tempdir();
        let path = dir.path().join("result.json");
        let mut json = String::from(r#"{"name":"Big","id":1,"messages":["#);
        for i in 0..(SCAN_ITEM_CAP + 10) {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"id":{i},"type":"message","date":"2026-11-01T10:00:00","from":"A","text":"m{i}"}}"#
            ));
        }
        json.push_str("]}");
        write(&path, &json);
        let preview = preview_import(&path).expect("big preview");
        assert!(!preview.scan_complete);
        assert_eq!(preview.messages_seen, SCAN_ITEM_CAP);
        assert!(preview.warnings.iter().any(|w| w.contains("lower bound")));
    }

    #[test]
    fn unsupported_and_missing_inputs_fail_with_guidance() {
        let dir = tempdir();
        let junk = dir.path().join("notes.txt");
        write(&junk, "just some notes\nwith no export shape\n");
        let err = preview_import(&junk).unwrap_err().to_string();
        assert!(
            err.contains("unrecognized"),
            "unsupported input explains itself: {err}"
        );

        let missing = dir.path().join("nope.mbox");
        let err = preview_import(&missing).unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "missing input says not found: {err}"
        );
    }

    #[test]
    fn zip_pick_previews_the_expanded_tree() {
        let dir = tempdir();
        let inner = dir.path().join("inner");
        std::fs::create_dir_all(inner.join("photos")).unwrap();
        let result_json = r#"{"name":"Z","id":1,"messages":[
            {"id":1,"type":"message","date":"2026-11-01T10:00:00","from":"A","text":"z","photo":"photos/p.jpg"}
        ]}"#;
        write(&inner.join("result.json"), result_json);
        write(&inner.join("photos").join("p.jpg"), "jpegbytes");
        let zip_path = dir.path().join("export.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in [
            ("result.json", result_json.as_bytes().to_vec()),
            ("photos/p.jpg", b"jpegbytes".to_vec()),
        ] {
            zip.start_file(name, zip::write::FileOptions::<()>::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let preview = preview_import(&zip_path).expect("zip preview");
        assert_eq!(preview.kind, "telegram");
        assert_eq!(preview.messages_seen, 1);
        assert_eq!(preview.media_missing, 0, "photo present inside the zip");
    }
}
