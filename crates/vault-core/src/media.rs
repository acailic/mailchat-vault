//! Media extraction for chat exports: files referenced by WhatsApp and
//! Telegram exports are copied into a content-addressed `media/` store,
//! and each message's `raw_json` gains a `media` array linking to them.
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
//!                "size": 12345 } ] }
//! ```
//!
//! The pass is a post-processing step over collected messages: the parsers
//! stay untouched, and a text-only export (or a bare `result.json` without
//! its sibling files) simply links nothing — `MediaSummary::missing`
//! counts the references that found no file. Messages already in the
//! archive (dedup skip) get their `media` array backfilled by
//! [`crate::ingest::ingest_collected`], so re-importing an export with
//! media present enriches an existing archive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::import_bounds::{safe_child, walk_bounded, ImportBounds};
use crate::message::NewMessage;
use crate::object_store::{Capacity, ObjectStore};
use crate::Result;

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
    /// bare `result.json`, path escapes).
    pub missing: u64,
    /// Messages whose `raw_json` media links were written or backfilled.
    /// Filled by [`crate::ingest::ingest_collected`].
    pub linked: u64,
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

/// Extract media for a Telegram export: `photo` and `file` string fields
/// hold paths relative to the export directory.
///
/// `export_root` is the directory containing `result.json`. Paths are
/// resolved and confined to it (`..` and symlink escapes count as
/// missing); the whole original message object stays in `raw_json` with
/// only the `media` array added.
pub fn extract_telegram(
    messages: &mut [NewMessage],
    export_root: &Path,
    data_dir: &Path,
    summary: &mut MediaSummary,
    budget: &mut Capacity,
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
        let mut entries = Vec::new();
        for (rel, media_type) in refs {
            match safe_child(export_root, &rel) {
                Some(src) => {
                    let name = file_display_name(&rel);
                    let kind = media_kind(media_type.as_deref(), &name);
                    match store(data_dir, &src, &name, &kind, budget) {
                        Ok((entry, written)) => {
                            if written {
                                summary.stored += 1;
                            } else {
                                summary.deduped += 1;
                            }
                            entries.push(entry);
                        }
                        // Unreadable file: keep importing, count as missing.
                        Err(_) => summary.missing += 1,
                    }
                }
                None => summary.missing += 1,
            }
        }
        if !entries.is_empty() {
            obj.insert("media".to_string(), serde_json::to_value(&entries)?);
            msg.raw_json = raw.to_string();
        }
    }
    Ok(())
}

/// The `photo`/`file` references of one Telegram message object, in a
/// stable order: `(relative path, media_type hint)`. The hint rides only
/// on the `file` ref — `photo` is always the JPEG thumbnail, so its kind
/// comes from its own extension.
fn telegram_refs(obj: &Map<String, Value>) -> Vec<(String, Option<String>)> {
    let media_type = obj
        .get("media_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut refs = Vec::new();
    for field in ["photo", "file"] {
        if let Some(rel) = obj.get(field).and_then(Value::as_str) {
            if !rel.is_empty() {
                let hint = (field == "file").then(|| media_type.clone()).flatten();
                refs.push((rel.to_string(), hint));
            }
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
        let Some(src) = lookup_attachment(&by_name, &by_stem, name).cloned() else {
            summary.missing += 1;
            continue;
        };
        let kind_hint = attachment.get("kind").and_then(Value::as_str);
        let kind = media_kind(kind_hint, name);
        match store(data_dir, &src, name, &kind, budget) {
            Ok((entry, written)) => {
                if written {
                    summary.stored += 1;
                } else {
                    summary.deduped += 1;
                }
                obj.insert("media".to_string(), serde_json::to_value(&[entry])?);
                msg.raw_json = raw.to_string();
            }
            Err(_) => summary.missing += 1,
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
