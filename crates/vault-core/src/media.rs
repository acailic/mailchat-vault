//! Media extraction for chat exports: files referenced by WhatsApp and
//! Telegram exports are copied into a content-addressed `media/` store,
//! and each message's `raw_json` gains a `media` array linking to them.
//!
//! Store layout: `<data_dir>/media/<sha256-hex>.<ext>` — the file name is the
//! SHA-256 of the contents, so identical files (re-imports, the same photo
//! in two chats) are stored once and never duplicated. `raw_json` keeps
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
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::message::NewMessage;
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

/// Copy `src` into the content-addressed store at `data_dir/media/`.
///
/// The copy is made first (through a temp file inside the store
/// directory) and then hashed streaming, so the recorded digest always
/// describes the stored bytes. The hash is the content's identity: if
/// any file named `<sha256>.*` is already in the store — under this
/// extension or another — it is reused, so identical files (re-imports,
/// the same photo in two chats, the same bytes under `photo.jpg` and
/// `photo.jpeg`) are stored once. Returns the store entry with the
/// original display name and kind plus whether this call wrote the
/// file (`false` = identical content was already stored); `path` is
/// relative to `data_dir` (`media/<sha256>.<ext>`), so it stays valid
/// wherever app-data lives.
pub fn store(
    data_dir: &Path,
    src: &Path,
    display_name: &str,
    kind: &str,
) -> Result<(MediaEntry, bool)> {
    let store_dir = data_dir.join("media");
    fs::create_dir_all(&store_dir)?;
    // Same name means same content; writing through a temp file keeps a
    // half-written copy out of the store, and hashing the copy (not the
    // source) keeps the digest honest if the source changes mid-import.
    let tmp = store_dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let (sha256, size) = match fs::copy(src, &tmp) {
        Ok(_) => hash_file(&tmp).inspect_err(|_| {
            fs::remove_file(&tmp).ok();
        })?,
        Err(e) => {
            fs::remove_file(&tmp).ok();
            return Err(e.into());
        }
    };
    // The hash is the identity: reuse an existing entry for the same
    // content even when it is stored under a different extension. The
    // exact `<sha256>.<ext>` name covers the common hits (re-imports,
    // same bytes under a stable name) without enumerating the store.
    let ext = sanitize_extension(display_name);
    let exact_name = format!("{sha256}.{ext}");
    let file_name = if store_dir.join(&exact_name).exists() {
        exact_name
    } else {
        existing_store_name(&store_dir, &sha256).unwrap_or(exact_name)
    };
    let target = store_dir.join(&file_name);
    let mut written = false;
    if target.exists() {
        fs::remove_file(&tmp).ok();
    } else {
        written = true;
        // Another import may have written the same content meanwhile; an
        // atomic rename wins, and a collision that is not the same content
        // (cryptographically impossible in practice) fails loudly.
        if let Err(e) = fs::rename(&tmp, &target) {
            let same_content = matches!(hash_file(&target), Ok((h, _)) if h == sha256);
            fs::remove_file(&tmp).ok();
            if !same_content {
                return Err(e.into());
            }
        }
    }
    Ok((
        MediaEntry {
            sha256,
            path: format!("media/{file_name}"),
            name: display_name.to_string(),
            kind: kind.to_string(),
            size,
        },
        written,
    ))
}

/// The name of an existing store file for this content, if any: the first
/// file whose name starts with the hash (the extension varies, the hash
/// does not).
fn existing_store_name(store_dir: &Path, sha256: &str) -> Option<String> {
    fs::read_dir(store_dir)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_type().map(|t| t.is_file()).unwrap_or(false)
                && e.file_name().to_string_lossy().starts_with(sha256)
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
}

/// Streaming SHA-256 + file size.
fn hash_file(path: &Path) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

/// Lowercase extension, kept when alphanumeric and reasonably short;
/// anything else (no extension, weird suffixes) becomes `bin`.
fn sanitize_extension(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let ok = !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        ext.to_ascii_lowercase()
    } else {
        "bin".to_string()
    }
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
            match resolve_within(export_root, &rel) {
                Some(src) => {
                    let name = file_display_name(&rel);
                    let kind = media_kind(media_type.as_deref(), &name);
                    match store(data_dir, &src, &name, &kind) {
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
/// stable order: `(relative path, media_type hint)`.
fn telegram_refs(obj: &Map<String, Value>) -> Vec<(String, Option<String>)> {
    let media_type = obj
        .get("media_type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut refs = Vec::new();
    for field in ["photo", "file"] {
        if let Some(rel) = obj.get(field).and_then(Value::as_str) {
            if !rel.is_empty() {
                refs.push((rel.to_string(), media_type.clone()));
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
/// the world: at most [`MAX_WALK_ENTRIES`] entries and depth 6, hidden
/// directories skipped.
pub fn extract_whatsapp(
    messages: &mut [NewMessage],
    export_root: &Path,
    data_dir: &Path,
    summary: &mut MediaSummary,
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
    if !wanted.is_empty() {
        let mut lower_wanted: Vec<String> = wanted.iter().map(|w| w.to_lowercase()).collect();
        lower_wanted.sort();
        lower_wanted.dedup();
        collect_names(export_root, 0, &mut 0, &mut by_name, &lower_wanted);
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
        let Some(src) = lookup_attachment(&by_name, name).cloned() else {
            summary.missing += 1;
            continue;
        };
        let kind_hint = attachment.get("kind").and_then(Value::as_str);
        let kind = media_kind(kind_hint, name);
        match store(data_dir, &src, name, &kind) {
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

/// Find the on-disk file for an export attachment name: exact, then by
/// stem (export flavors sometimes re-encode `IMG-1.jpg` as `IMG-1.jpeg`),
/// then the same two case-insensitively.
fn lookup_attachment<'a>(by_name: &'a HashMap<String, PathBuf>, name: &str) -> Option<&'a PathBuf> {
    if let Some(hit) = by_name.get(name) {
        return Some(hit);
    }
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if let Some(hit) = by_name.get(stem) {
        return Some(hit);
    }
    let lower = name.to_lowercase();
    let lower_stem = stem.to_lowercase();
    by_name
        .iter()
        .find(|(k, _)| k.to_lowercase() == lower)
        .or_else(|| by_name.iter().find(|(k, _)| k.to_lowercase() == lower_stem))
        .map(|(_, v)| v)
}

/// Most files a WhatsApp export lookup will walk before giving up.
const MAX_WALK_ENTRIES: usize = 20_000;
/// How deep under the export root media files are searched.
const MAX_WALK_DEPTH: usize = 6;

/// Record `path` under its file name when it matches a wanted name
/// (exact or ASCII-case-insensitive). Bounded depth and entry count.
fn collect_names(
    dir: &Path,
    depth: usize,
    seen: &mut usize,
    by_name: &mut HashMap<String, PathBuf>,
    lower_wanted: &[String],
) {
    if depth > MAX_WALK_DEPTH || *seen > MAX_WALK_ENTRIES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        *seen += 1;
        if *seen > MAX_WALK_ENTRIES {
            return;
        }
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') {
                collect_names(&path, depth + 1, seen, by_name, lower_wanted);
            }
        } else if file_type.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.rsplit_once('.') {
                // Match either the full name or the stem: some export
                // flavors rename IMG-….jpg to IMG-….jpeg on disk.
                let lower = name.to_lowercase();
                let lower_stem = stem.0.to_lowercase();
                if lower_wanted.binary_search(&lower).is_ok()
                    || lower_wanted.binary_search(&lower_stem).is_ok()
                {
                    by_name.entry(name.clone()).or_insert(path.clone());
                    by_name.entry(stem.0.to_string()).or_insert(path);
                }
            } else {
                let lower = name.to_lowercase();
                if lower_wanted.binary_search(&lower).is_ok() {
                    by_name.entry(name.clone()).or_insert(path);
                }
            }
        }
    }
}

/// Resolve `rel` inside `root` and confirm the result stays inside:
/// lexical normalization rejects `..` components, then the canonical
/// path must still be under the canonical root (symlinks).
fn resolve_within(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    for component in rel_path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return None, // ParentDir, CurDir, root, or prefix
        }
    }
    let joined = root.join(rel_path);
    let canonical = joined.canonicalize().ok()?;
    let canonical_root = root.canonicalize().ok()?;
    if canonical.starts_with(&canonical_root) && canonical != canonical_root {
        Some(canonical)
    } else {
        None
    }
}

/// Display name for a Telegram relative path: the final component.
fn file_display_name(rel: &str) -> String {
    rel.rsplit(['/', '\\']).next().unwrap_or(rel).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (first, written) = store(&data_root, &src, "photo.jpg", "image").unwrap();
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
        let (second, written) = store(&data_root, &other, "renamed.jpg", "image").unwrap();
        assert!(!written, "identical content is not rewritten");
        assert_eq!(second.sha256, first.sha256);
        assert_eq!(second.path, first.path);
        assert_eq!(second.name, "renamed.jpg");

        // Different content → different hash and a second file.
        fs::write(&other, b"other-bytes").unwrap();
        let (third, written) = store(&data_root, &other, "renamed.jpg", "image").unwrap();
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
        let (first, written) = store(&data_root, &as_jpg, "photo.jpg", "image").unwrap();
        assert!(written);
        let (second, written) = store(&data_root, &as_jpeg, "photo.jpeg", "image").unwrap();
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

    #[test]
    fn resolve_within_confines_to_root() {
        let dir = tmpdir("resolve");
        let sub = dir.join("photos");
        fs::create_dir(&sub).unwrap();
        let file = sub.join("p.jpg");
        fs::write(&file, b"x").unwrap();

        assert_eq!(resolve_within(&dir, "photos/p.jpg"), Some(file.clone()));
        assert_eq!(resolve_within(&dir, "photos/missing.jpg"), None);
        assert_eq!(resolve_within(&dir, "../escape.jpg"), None);
        assert_eq!(resolve_within(&dir, "/etc/passwd"), None);
        assert_eq!(resolve_within(&dir, "."), None);

        // A symlink pointing outside the root is rejected after
        // canonicalization.
        #[cfg(unix)]
        {
            let outside = dir.parent().unwrap().join("mcv-outside-target.txt");
            fs::write(&outside, b"out").unwrap();
            let link = dir.join("leak.txt");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert_eq!(resolve_within(&dir, "leak.txt"), None);
            let _ = fs::remove_file(&outside);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
