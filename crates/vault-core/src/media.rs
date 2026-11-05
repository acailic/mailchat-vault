//! Media extraction (M4.2): every file a message references is copied
//! into the content-addressed `media/` store, linked from the message's
//! `raw_json` — and every reference whose file is absent is recorded as
//! an explicit **missing** entry, so the archive never silently drops a
//! reference it could not preserve.
//!
//! Store layout: `<data_dir>/media/<sha256-hex>.<ext>` — the file name is the
//! SHA-256 of the contents, so identical files (re-imports, the same photo
//! in two chats) are stored once and never duplicated. Writes go through
//! the M4.1 stage → promote → reference lifecycle of
//! [`crate::object_store`] (staging lives under `media/.staging/`, never
//! visible as objects; see that module for the crash-safety contract and
//! the reconciliation tool). `raw_json` keeps
//! the original display name, kind, size, and the store-relative path:
//!
//! ```json
//! { "media": [ { "sha256": "…", "path": "media/<sha256>.<ext>",
//!                "name": "photo_1@01-11-2023.jpg", "kind": "image",
//!                "size": 12345 },
//!              { "name": "photo_2@01-11-2023.jpg", "kind": "image",
//!                "missing": true, "reason": "not-in-export" } ] }
//! ```
//!
//! A missing entry carries no digest and no path — it says exactly what
//! the archive knows: this message referenced a file with this name,
//! and the bytes are not preserved. Re-importing the export with its
//! files backfills the real entry in place of the missing one
//! (see [`crate::storage::Storage::attach_media`]).
//!
//! The chat passes are post-processing steps over collected messages:
//! the parsers stay untouched. Email attachment bytes are decoded by
//! the mail parser ([`crate::mail`]) and staged here through
//! [`MailMedia`] (the IMAP sync path) or the bounded mail pass (mbox).
//! Files that sit in an export tree but belong to no message are
//! stored too when the import picked the whole tree (directory or
//! `.zip` picks — never a bare export file, whose parent directory is
//! not asserted as export content) and registered in the `media_objects`
//! registry so they are referenced, visible, and re-import-safe.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::import_bounds::{safe_child, walk_bounded, ImportBounds};
use crate::message::NewMessage;
use crate::object_store::{Capacity, ObjectStore};
use crate::Result;

/// Why a referenced file was not preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingReason {
    /// The reference resolved to no file inside the export tree (a
    /// text-only export, a bare `result.json`, a path escape).
    NotInExport,
    /// The file exists but its bytes could not be read or staged.
    Unreadable,
}

impl MissingReason {
    fn as_str(self) -> &'static str {
        match self {
            MissingReason::NotInExport => "not-in-export",
            MissingReason::Unreadable => "unreadable",
        }
    }
}

/// How the extraction pass went, for the import status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSummary {
    /// Messages that referenced at least one media file.
    pub referenced: u64,
    /// Files newly written into the store.
    pub stored: u64,
    /// Files whose content was already in the store (hash match).
    pub deduped: u64,
    /// Referenced files not present in the export (text-only exports,
    /// bare `result.json`, path escapes). Each is recorded as a missing
    /// `media` entry on its message, not dropped silently.
    pub missing: u64,
    /// Messages whose `raw_json` media links were written or backfilled.
    /// Filled by [`crate::ingest::ingest_collected`].
    pub linked: u64,
    /// Export-tree files no message references, newly stored (M4.2;
    /// directory and `.zip` picks only).
    #[serde(default)]
    pub unlinked: u64,
    /// Unlinked files whose content was already in the store.
    #[serde(default)]
    pub unlinked_deduped: u64,
}

/// The `raw_json` `media` array element (also the IPC shape the frontend
/// renders in the detail view).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaEntry {
    pub sha256: String,
    /// Store-relative path, `media/<sha256>.<ext>`.
    pub path: String,
    /// Original file name from the export.
    pub name: String,
    /// `image`, `video`, `audio`, `sticker`, `document`, …
    pub kind: String,
    /// File size in bytes.
    pub size: u64,
}

/// One unlinked object staged for the `media_objects` registry (M4.2):
/// the digest and provenance of a file that sits in the export tree but
/// belongs to no message. Registered in the same transaction as the
/// import's final batch, so reconciliation counts it as referenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlinkedObject {
    pub sha256: String,
    pub size: u64,
    /// Original file name from the export tree.
    pub name: String,
    /// Coarse kind, from the file's extension.
    pub kind: String,
}

/// Copy `src` into the content-addressed store at `data_dir/media/`
/// under the M4.1 lifecycle: stage into `media/.staging/` while hashing
/// (bytes charged against `budget`), then atomically promote to
/// `media/<sha256>.<ext>` (`rename(2)`), then — the caller's step — the
/// digest is referenced from the message's `raw_json` when the ingest
/// batch commits. A crash at any point leaves no half-written object
/// visible and no dangling reference (see [`crate::object_store`]).
///
/// The hash is the content's identity: if any file named `<sha256>.*`
/// is already in the store — under this extension or another — it is
/// reused (and the budget refunded), so identical files (re-imports,
/// the same photo in two chats, the same bytes under `photo.jpg` and
/// `photo.jpeg`) are stored once. The stored object is a byte-exact
/// copy of the input (input-original preservation); `path` is relative
/// to `data_dir` (`media/<sha256>.<ext>`), so it stays valid wherever
/// app-data lives. Returns the store entry with the original display
/// name and kind plus whether this call wrote the file (`false` =
/// identical content was already stored).
pub fn store(
    data_dir: &Path,
    src: &Path,
    display_name: &str,
    kind: &str,
    budget: &mut Capacity,
) -> Result<(MediaEntry, bool)> {
    let object_store = ObjectStore::new(data_dir);
    let ext = sanitize_extension(display_name);
    let staged = object_store.stage(src, &ext, budget)?;
    let object = object_store.promote(staged, budget)?;
    Ok((
        MediaEntry {
            sha256: object.sha256,
            path: object.path,
            name: display_name.to_string(),
            kind: kind.to_string(),
            size: object.size,
        },
        object.written,
    ))
}

/// Lowercase extension of a display name, kept when alphanumeric and
/// reasonably short; anything else (no extension, weird suffixes)
/// becomes `bin` (the rule lives in
/// [`object_store::sanitize_ext`](crate::object_store::sanitize_ext)).
fn sanitize_extension(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    crate::object_store::sanitize_ext(ext)
}

/// The kind of a media file, from the export's own hint (WhatsApp
/// placeholder kind, Telegram `media_type`) with the file extension as
/// fallback.
pub fn media_kind(hint: Option<&str>, name: &str) -> String {
    // Generic WhatsApp placeholder kinds say nothing about the actual
    // file; the extension knows better.
    const GENERIC: [&str; 3] = ["file", "media", "document"];
    if let Some(hint) = hint.filter(|h| !h.is_empty() && !GENERIC.contains(h)) {
        let normalized = match hint {
            "photo" => Some("image"),
            "video_file" | "video_message" => Some("video"),
            "voice_message" | "audio_file" => Some("audio"),
            "animation" => Some("gif"),
            _ => None,
        };
        if let Some(kind) = normalized {
            return kind.to_string();
        }
        return hint.to_string();
    }
    let ext = sanitize_extension(name);
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tiff" => {
            "image".to_string()
        }
        "mp4" | "mov" | "3gp" | "webm" | "mkv" | "avi" => "video".to_string(),
        "opus" | "ogg" | "m4a" | "mp3" | "aac" | "amr" | "wav" | "flac" => "audio".to_string(),
        "webp_sticker" | "tgs" | "webm_sticker" => "sticker".to_string(),
        _ => "document".to_string(),
    }
}

/// Build the `raw_json` value of one missing entry.
fn missing_entry_value(name: &str, kind: &str, reason: MissingReason) -> Value {
    serde_json::json!({
        "name": name,
        "kind": kind,
        "missing": true,
        "reason": reason.as_str(),
    })
}

/// Append `entry` to a message's `raw_json` `media` array (creating the
/// array when absent). Leaves non-object `raw_json` alone.
fn push_media_entry(msg: &mut NewMessage, entry: Value) {
    let Ok(mut raw) = serde_json::from_str::<Value>(&msg.raw_json) else {
        return;
    };
    let Some(obj) = raw.as_object_mut() else {
        return;
    };
    let media = obj
        .entry("media".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(array) = media.as_array_mut() {
        array.push(entry);
        msg.raw_json = raw.to_string();
    }
}

/// Extract media for a Telegram export: the explicit file references a
/// message carries — `photo` and `file`, as path strings (the classic
/// shape) or as objects with a nested `file`/`photo` string (newer
/// exporters), with `file_name` naming the part when the export records
/// one — resolve against the export directory and are copied into the
/// store. A reference whose file is absent is recorded as a missing
/// entry, never dropped.
///
/// `export_root` is the directory containing `result.json`. Paths are
/// resolved and confined to it (`..` and symlink escapes count as
/// missing); the whole original message object stays in `raw_json` with
/// only the `media` array added. `consumed` collects the absolute paths
/// this pass stored, so the unlinked pass can tell export files that
/// belong to a message from files that belong to no message.
pub fn extract_telegram(
    messages: &mut [NewMessage],
    export_root: &Path,
    data_dir: &Path,
    summary: &mut MediaSummary,
    budget: &mut Capacity,
    consumed: &mut HashSet<PathBuf>,
) -> Result<()> {
    for msg in messages.iter_mut() {
        let mut raw: Value =
            serde_json::from_str(&msg.raw_json).unwrap_or_else(|_| Value::Object(Map::new()));
        let Some(obj) = raw.as_object_mut() else {
            continue;
        };
        let refs = telegram_refs(obj);
        if refs.is_empty() {
            continue;
        }
        summary.referenced += 1;
        let mut entries: Vec<Value> = Vec::new();
        for (rel, media_type, display) in refs {
            let name = display.unwrap_or_else(|| file_display_name(&rel));
            let kind = media_kind(media_type.as_deref(), &name);
            match safe_child(export_root, &rel) {
                Some(src) => match store(data_dir, &src, &name, &kind, budget) {
                    Ok((entry, written)) => {
                        if written {
                            summary.stored += 1;
                        } else {
                            summary.deduped += 1;
                        }
                        consumed.insert(src);
                        entries.push(serde_json::to_value(&entry)?);
                    }
                    // Unreadable file: keep importing, record why.
                    Err(_) => {
                        summary.missing += 1;
                        entries.push(missing_entry_value(&name, &kind, MissingReason::Unreadable));
                    }
                },
                None => {
                    summary.missing += 1;
                    entries.push(missing_entry_value(
                        &name,
                        &kind,
                        MissingReason::NotInExport,
                    ));
                }
            }
        }
        if !entries.is_empty() {
            obj.insert("media".to_string(), Value::Array(entries));
            msg.raw_json = raw.to_string();
        }
    }
    Ok(())
}

/// The `photo`/`file` references of one Telegram message object, in a
/// stable order: `(relative path, media_type hint, explicit display
/// name)`. The hint rides only on the `file` ref — `photo` is always the
/// JPEG thumbnail, so its kind comes from its own extension. Newer
/// exporters write these fields as objects carrying a nested `file`
/// string (plus `file_name`); both shapes resolve to the same explicit
/// reference.
fn telegram_refs(obj: &Map<String, Value>) -> Vec<(String, Option<String>, Option<String>)> {
    let media_type = obj
        .get("media_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let file_name = obj
        .get("file_name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    let mut refs = Vec::new();
    for field in ["photo", "file"] {
        match obj.get(field) {
            Some(Value::String(rel)) if !rel.is_empty() => {
                let hint = (field == "file").then(|| media_type.clone()).flatten();
                let display = (field == "file").then(|| file_name.clone()).flatten();
                refs.push((rel.clone(), hint, display));
            }
            Some(Value::Object(inner)) => {
                // Newer export shape: the field is an object whose
                // `file`/`photo` member is the path.
                for nested in ["file", "photo"] {
                    if let Some(Value::String(rel)) = inner.get(nested) {
                        if !rel.is_empty() {
                            let hint = (field == "file").then(|| media_type.clone()).flatten();
                            let display = (field == "file").then(|| file_name.clone()).flatten();
                            refs.push((rel.clone(), hint, display));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    refs
}

/// Extract media for a WhatsApp export: attachment names parsed from
/// `(file attached)` placeholders are matched against files next to the
/// export `.txt` (recursively, since some exports nest media folders).
///
/// The walk only runs when the export actually references named files,
/// and is bounded so a `.txt` picked from a huge directory cannot scan
/// the world: the M4.1 import bounds ([`crate::import_bounds`]) cap
/// files, total bytes, and depth, skip hidden directories and every
/// symlink, and refuse a tree that crosses a bound instead of quietly
/// missing media.
pub fn extract_whatsapp(
    messages: &mut [NewMessage],
    export_root: &Path,
    data_dir: &Path,
    summary: &mut MediaSummary,
    budget: &mut Capacity,
    consumed: &mut HashSet<PathBuf>,
) -> Result<()> {
    let wanted: Vec<String> = messages
        .iter()
        .filter_map(|m| {
            let raw: Value = serde_json::from_str(&m.raw_json).ok()?;
            raw.get("attachment")
                .and_then(|a| a.get("name"))
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
        })
        .collect();
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    let mut by_stem: HashMap<String, PathBuf> = HashMap::new();
    if !wanted.is_empty() {
        let mut lower_wanted: Vec<String> = wanted.iter().map(|w| w.to_lowercase()).collect();
        lower_wanted.sort();
        lower_wanted.dedup();
        let mut lower_wanted_stems: Vec<String> =
            wanted.iter().map(|w| stem_of(w).to_lowercase()).collect();
        lower_wanted_stems.sort();
        lower_wanted_stems.dedup();
        // One bounded walk (M4.1) collects every candidate file under
        // the shared traversal/symlink/capacity fence; the lookup maps
        // keep only the wanted names and stems.
        for file in walk_bounded(export_root, &ImportBounds::default())? {
            let Some(name) = file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let lower = name.to_lowercase();
            if lower_wanted.binary_search(&lower).is_ok() {
                by_name.entry(lower).or_insert(file.path.clone());
            }
            let lower_stem = stem_of(&name).to_lowercase();
            if lower_wanted_stems.binary_search(&lower_stem).is_ok() {
                by_stem.entry(lower_stem).or_insert(file.path);
            }
        }
    }

    for msg in messages.iter_mut() {
        let mut raw: Value = serde_json::from_str(&msg.raw_json)?;
        let Some(obj) = raw.as_object_mut() else {
            continue;
        };
        let Some(attachment) = obj.get("attachment").cloned() else {
            continue;
        };
        let Some(name) = attachment.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        summary.referenced += 1;
        let kind_hint = attachment.get("kind").and_then(Value::as_str);
        let kind = media_kind(kind_hint, name);
        let Some(src) = lookup_attachment(&by_name, &by_stem, name).cloned() else {
            // No file in the export answers for the name: the reference
            // is recorded as missing, not silently dropped (M4.2).
            summary.missing += 1;
            let entry = missing_entry_value(name, &kind, MissingReason::NotInExport);
            let media = obj
                .entry("media".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(array) = media.as_array_mut() {
                array.push(entry);
                msg.raw_json = raw.to_string();
            }
            continue;
        };
        match store(data_dir, &src, name, &kind, budget) {
            Ok((entry, written)) => {
                if written {
                    summary.stored += 1;
                } else {
                    summary.deduped += 1;
                }
                consumed.insert(src);
                obj.insert("media".to_string(), serde_json::to_value(&[entry])?);
                msg.raw_json = raw.to_string();
            }
            Err(_) => {
                summary.missing += 1;
                let entry = missing_entry_value(name, &kind, MissingReason::Unreadable);
                let media = obj
                    .entry("media".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(array) = media.as_array_mut() {
                    array.push(entry);
                    msg.raw_json = raw.to_string();
                }
            }
        }
    }
    Ok(())
}

/// Find the on-disk file for an export attachment name: by full name
/// first (ASCII-case-insensitively), then by matching stem-of-disk-file
/// against stem-of-reference — the fallback for export flavors that
/// re-encode `IMG-1.jpg` as `IMG-1.jpeg`. A disk file's stem is never
/// compared against a reference's full name, so `plan.pdf.bak` cannot
/// answer for a missing `plan.pdf`.
fn lookup_attachment<'a>(
    by_name: &'a HashMap<String, PathBuf>,
    by_stem: &'a HashMap<String, PathBuf>,
    name: &str,
) -> Option<&'a PathBuf> {
    if let Some(hit) = by_name.get(&name.to_lowercase()) {
        return Some(hit);
    }
    by_stem.get(&stem_of(name).to_lowercase())
}

/// The stem of a file name: everything before the last dot; a name
/// without a dot is its own stem.
fn stem_of(name: &str) -> &str {
    name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name)
}

/// Display name for a Telegram relative path: the final component.
fn file_display_name(rel: &str) -> String {
    rel.rsplit(['/', '\\']).next().unwrap_or(rel).to_string()
}

/// Export-artifact files the unlinked pass never stores: HTML/JSON
/// sidecars the exporter itself writes (pages, contact tables), which
/// are not user media.
fn is_export_artifact(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered.ends_with(".html")
        || lowered.ends_with(".htm")
        || lowered.ends_with(".json")
        || lowered == "desktop.ini"
        || lowered == "thumbs.db"
}

/// Store the export tree's **unlinked** files (M4.2): everything under
/// `export_root` that no message referenced (`consumed` — the paths the
/// message passes stored) and that is neither the export file itself nor
/// an exporter artifact. The caller only enables this for imports that
/// picked the whole tree (a directory or a `.zip`): a bare export file's
/// parent directory is not asserted as export content, and copying it
/// into the vault would be a surprise, not an archive.
///
/// Returns the objects to register in the `media_objects` registry so
/// they are referenced (reconciliation-clean) and re-import-safe (the
/// registry upserts on the digest).
pub fn extract_unlinked(
    export_root: &Path,
    export_file: Option<&Path>,
    data_dir: &Path,
    consumed: &HashSet<PathBuf>,
    linked_digests: &HashSet<String>,
    summary: &mut MediaSummary,
    budget: &mut Capacity,
) -> Result<Vec<UnlinkedObject>> {
    let mut records = Vec::new();
    if !export_root.is_dir() {
        return Ok(records);
    }
    for file in walk_bounded(export_root, &ImportBounds::default())? {
        if file.path.is_dir() {
            continue;
        }
        if Some(file.path.as_path()) == export_file {
            continue;
        }
        if consumed.contains(&file.path) {
            continue;
        }
        let Some(name) = file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            continue;
        };
        if is_export_artifact(&name) {
            continue;
        }
        let kind = media_kind(None, &name);
        match store(data_dir, &file.path, &name, &kind, budget) {
            Ok((entry, written)) => {
                if linked_digests.contains(&entry.sha256) {
                    // The content is message-linked — already preserved
                    // and reachable from its message (this run's or an
                    // interrupted earlier run's batches). Not unlinked.
                    if written {
                        // First copy in the vault; it stays, but it is a
                        // linked object's bytes, not an unlinked one.
                        summary.stored += 1;
                    }
                    continue;
                }
                if written {
                    summary.unlinked += 1;
                } else {
                    summary.unlinked_deduped += 1;
                }
                records.push(UnlinkedObject {
                    sha256: entry.sha256,
                    size: entry.size,
                    name: entry.name,
                    kind: entry.kind,
                });
            }
            // An unreadable stray file is skipped: there is no message
            // to record a missing entry on, and the import must not
            // fail over export litter.
            Err(_) => continue,
        }
    }
    Ok(records)
}

/// The mail media pass handle: stores one parsed mail's attachment
/// bytes ([`crate::message::NewMessage::attachments`]) into the object
/// store and records the `media` entries in the message's `raw_json`.
/// Used by the bounded mail import (mbox) per batch and by the IMAP
/// sync right after parsing a fetched batch — on the calling thread,
/// never inside a storage transaction (the M4.1 lifecycle makes
/// promote-before-reference safe: a crash leaves a harmless orphan).
///
/// One [`Capacity`] budget should span a whole import or sync round;
/// dedup hits refund their charge, so re-fetches never eat into it.
pub struct MailMedia<'a> {
    pub data_dir: &'a Path,
    pub budget: &'a mut Capacity,
    /// Counters accumulated across all messages this pass stores.
    pub stored: u64,
    pub deduped: u64,
    /// References whose bytes could not be staged (budget, disk error):
    /// recorded as missing entries on the message, counted here.
    pub missing: u64,
}

impl<'a> MailMedia<'a> {
    pub fn new(data_dir: &'a Path, budget: &'a mut Capacity) -> Self {
        Self {
            data_dir,
            budget,
            stored: 0,
            deduped: 0,
            missing: 0,
        }
    }

    /// Store one message's attachments, then drop the transient bytes
    /// from the message (the persisted shape is the `raw_json` link).
    pub fn store_message(&mut self, msg: &mut NewMessage) {
        if msg.attachments.is_empty() {
            return;
        }
        let object_store = ObjectStore::new(self.data_dir);
        for attachment in std::mem::take(&mut msg.attachments) {
            let entry = store_bytes(
                &object_store,
                &attachment.bytes,
                &attachment.name,
                &attachment.kind,
                self.budget,
            );
            match entry {
                Ok((entry, written)) => {
                    if written {
                        self.stored += 1;
                    } else {
                        self.deduped += 1;
                    }
                    let _ = serde_json::to_value(&entry).map(|v| push_media_entry(msg, v));
                }
                Err(_) => {
                    self.missing += 1;
                    push_media_entry(
                        msg,
                        missing_entry_value(
                            &attachment.name,
                            &attachment.kind,
                            MissingReason::Unreadable,
                        ),
                    );
                }
            }
        }
    }
}

/// Stage in-memory bytes through the M4.1 lifecycle (used by the mail
/// attachment passes): stream into staging while hashing, promote
/// atomically, reuse existing identical content.
fn store_bytes(
    object_store: &ObjectStore,
    bytes: &[u8],
    display_name: &str,
    kind: &str,
    budget: &mut Capacity,
) -> Result<(MediaEntry, bool)> {
    let ext = crate::object_store::sanitize_ext(
        display_name.rsplit_once('.').map(|(_, e)| e).unwrap_or(""),
    );
    let mut reader = std::io::Cursor::new(bytes);
    let staged = object_store.stage_reader(&mut reader, &ext, budget)?;
    let object = object_store.promote(staged, budget)?;
    Ok((
        MediaEntry {
            sha256: object.sha256,
            path: object.path,
            name: display_name.to_string(),
            kind: kind.to_string(),
            size: object.size,
        },
        object.written,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcv-media-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_is_content_addressed_and_idempotent() {
        let dir = tmpdir("store");
        let src = dir.join("photo.jpg");
        fs::write(&src, b"jpeg-bytes-here").unwrap();
        let data_root = dir.join("root");
        let (first, written) = store(
            &data_root,
            &src,
            "photo.jpg",
            "image",
            &mut Capacity::default(),
        )
        .unwrap();
        assert!(written, "first store writes the file");
        assert_eq!(first.name, "photo.jpg");
        assert_eq!(first.kind, "image");
        assert_eq!(first.size, 15);
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(first.path.matches('.').count(), 1);
        assert!(first.path.starts_with("media/"));
        assert!(first.path.ends_with(".jpg"));
        // The file landed at the hashed path.
        assert!(data_root.join(&first.path).is_file());

        // Same content under a different name: same hash → deduped.
        let other = dir.join("renamed.jpg");
        fs::write(&other, b"jpeg-bytes-here").unwrap();
        let (second, written) = store(
            &data_root,
            &other,
            "renamed.jpg",
            "image",
            &mut Capacity::default(),
        )
        .unwrap();
        assert!(!written, "identical content is not rewritten");
        assert_eq!(second.sha256, first.sha256);
        assert_eq!(second.path, first.path);
        assert_eq!(second.name, "renamed.jpg");

        // Different content → different hash and a second file.
        fs::write(&other, b"other-bytes").unwrap();
        let (third, written) = store(
            &data_root,
            &other,
            "renamed.jpg",
            "image",
            &mut Capacity::default(),
        )
        .unwrap();
        assert!(written);
        assert_ne!(third.sha256, first.sha256);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_content_under_different_extension_is_deduped() {
        let dir = tmpdir("dedup-ext");
        let as_jpg = dir.join("photo.jpg");
        let as_jpeg = dir.join("photo.jpeg");
        fs::write(&as_jpg, b"same-jpeg-bytes").unwrap();
        fs::write(&as_jpeg, b"same-jpeg-bytes").unwrap();
        let data_root = dir.join("root");
        let (first, written) = store(
            &data_root,
            &as_jpg,
            "photo.jpg",
            "image",
            &mut Capacity::default(),
        )
        .unwrap();
        assert!(written);
        let (second, written) = store(
            &data_root,
            &as_jpeg,
            "photo.jpeg",
            "image",
            &mut Capacity::default(),
        )
        .unwrap();
        assert!(!written, "same content under another extension is deduped");
        assert_eq!(second.sha256, first.sha256);
        assert_eq!(second.path, first.path, "one store file serves both names");
        assert_eq!(second.name, "photo.jpeg");
        let store_files = fs::read_dir(data_root.join("media"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().unwrap().is_file()
                    && !e.file_name().to_string_lossy().starts_with(".tmp-")
            })
            .count();
        assert_eq!(store_files, 1, "exactly one physical file in the store");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extensions_are_sanitized() {
        assert_eq!(sanitize_extension("IMG-1.JPG"), "jpg");
        assert_eq!(sanitize_extension("noext"), "bin");
        assert_eq!(sanitize_extension("a.very/long"), "bin");
        assert_eq!(sanitize_extension("a.tar.gz"), "gz");
    }

    #[test]
    fn kinds_come_from_hint_then_extension() {
        assert_eq!(media_kind(Some("photo"), "x"), "image");
        assert_eq!(media_kind(Some("video_file"), "x"), "video");
        assert_eq!(media_kind(Some("voice_message"), "x"), "audio");
        assert_eq!(media_kind(Some("custom"), "x"), "custom");
        assert_eq!(media_kind(None, "pic.png"), "image");
        assert_eq!(media_kind(None, "clip.MP4"), "video");
        assert_eq!(media_kind(None, "note.opus"), "audio");
        assert_eq!(media_kind(None, "plan.pdf"), "document");
        assert_eq!(media_kind(None, "mystery.xyz"), "document");
        // An empty hint falls through to the extension.
        assert_eq!(media_kind(Some(""), "x.jpg"), "image");
        // Generic WhatsApp placeholder kinds defer to the extension.
        assert_eq!(media_kind(Some("file"), "IMG-1.jpg"), "image");
        assert_eq!(media_kind(Some("media"), "clip.mp4"), "video");
        assert_eq!(media_kind(Some("document"), "plan.pdf"), "document");
    }
}
