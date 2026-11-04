//! M4.1: import bounds — the traversal / symlink / capacity fence every
//! import tree walk and ZIP expansion goes through.
//!
//! Importing "a directory of export files" or "a ZIP archive" means
//! trusting a tree of names the vault did not produce. This module is
//! the single audited path such input takes:
//!
//! - **Traversal** — a relative reference (`../`, absolute paths,
//!   Windows-style `..\` components) never resolves outside the import
//!   root ([`safe_child`]); a ZIP entry with such a name rejects the
//!   whole archive ([`expand_zip_bounded`]).
//! - **Symlinks** — a symlink inside an import tree (or a symlink entry
//!   in a ZIP) is never followed, and a reference that resolves through
//!   one out of the root is refused after canonicalization; a ZIP
//!   symlink/device/fifo entry rejects the archive.
//! - **Capacity** — file count, total bytes, and directory depth are
//!   bounded ([`ImportBounds`]), so a runaway tree or a ZIP bomb is
//!   rejected cleanly instead of filling the disk.
//!
//! The same [`ImportBounds`] constants govern the media object store's
//! staging budget ([`crate::object_store::Capacity`]).

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

/// Bounds for one import tree (directory walk or ZIP expansion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBounds {
    /// Most files one import may enumerate/extract.
    pub max_files: usize,
    /// Most total file bytes one import may enumerate/extract.
    pub max_total_bytes: u64,
    /// Deepest directory nesting under the import root.
    pub max_depth: usize,
}

impl Default for ImportBounds {
    fn default() -> Self {
        Self {
            // Generous for real personal exports; far below anything
            // that could hurt the machine. Historically the WhatsApp
            // media lookup walked with these exact entry/depth caps.
            max_files: 20_000,
            max_total_bytes: 32 * 1024 * 1024 * 1024, // 32 GiB
            max_depth: 6,
        }
    }
}

impl ImportBounds {
    fn check_file(&self, seen: usize, bytes: u64, size: u64) -> Result<()> {
        if seen + 1 > self.max_files {
            return Err(Error::ImportBounds(format!(
                "import tree exceeds the file-count bound: {} files (limit {})",
                seen + 1,
                self.max_files
            )));
        }
        if bytes + size > self.max_total_bytes {
            return Err(Error::ImportBounds(format!(
                "import tree exceeds the byte bound: {} bytes > limit {}",
                bytes + size,
                self.max_total_bytes
            )));
        }
        Ok(())
    }
}

/// Resolve `rel` (a `/`- or `\`-separated relative reference from an
/// export) inside `root` and confirm the result stays inside: every
/// component must be plain (`..`, `.`, absolute paths, and drive
/// prefixes are refused lexically), then the canonical path must still
/// sit under the canonical root (symlink escapes). Returns `None` for
/// anything that would leave the root or does not exist.
pub fn safe_child(root: &Path, rel: &str) -> Option<PathBuf> {
    let normalized = rel.replace('\\', "/");
    let rel_path = Path::new(&normalized);
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

/// One file found by a bounded walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedFile {
    /// Absolute path (inside the walked root; symlinks skipped, so the
    /// path never leaves the root).
    pub path: PathBuf,
    /// Path relative to the walk root, `/`-separated.
    pub rel: String,
    pub size: u64,
}

/// Enumerate the files under `root` within the bounds: symlinks (file
/// or directory) are skipped entirely, hidden directories are not
/// descended into, depth/file-count/byte totals are enforced. The walk
/// is read-only — bounds exist to fence what an import may look at,
/// not to modify the source tree.
pub fn walk_bounded(root: &Path, bounds: &ImportBounds) -> Result<Vec<WalkedFile>> {
    let mut ctx = WalkCtx {
        bounds,
        seen: 0,
        bytes: 0,
        dirs_seen: HashSet::new(),
    };
    let canonical_root = root.canonicalize().map_err(Error::from)?;
    let mut files = Vec::new();
    walk_rec(&canonical_root, "", 0, &mut ctx, &mut files)?;
    Ok(files)
}

/// Walk state shared across the recursion.
struct WalkCtx<'a> {
    bounds: &'a ImportBounds,
    seen: usize,
    bytes: u64,
    dirs_seen: HashSet<PathBuf>,
}

fn walk_rec(
    dir: &Path,
    rel_prefix: &str,
    depth: usize,
    ctx: &mut WalkCtx<'_>,
    files: &mut Vec<WalkedFile>,
) -> Result<()> {
    let bounds = ctx.bounds;
    if depth > bounds.max_depth {
        return Err(Error::ImportBounds(format!(
            "import tree nests deeper than the depth bound ({})",
            bounds.max_depth
        )));
    }
    // Resolve the physical directory and refuse symlink loops: every
    // descended directory must be a fresh physical path under the root.
    let canonical = dir.canonicalize().map_err(Error::from)?;
    if !ctx.dirs_seen.insert(canonical) {
        return Ok(()); // already walked: a bind/junction would loop otherwise
    }
    let entries = fs::read_dir(dir).map_err(Error::from)?;
    for entry in entries {
        let entry = entry.map_err(Error::from)?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Symlinks are never followed (a link is not its target; a
        // target outside the root must not enter the walk).
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            if name.starts_with('.') {
                continue; // hidden directories hold no export media
            }
            let rel = format!("{rel_prefix}{name}/");
            walk_rec(&entry.path(), &rel, depth + 1, ctx, files)?;
        } else if file_type.is_file() {
            let size = entry.metadata().map_err(Error::from)?.len();
            let rel = format!("{rel_prefix}{name}");
            bounds.check_file(ctx.seen, ctx.bytes, size)?;
            ctx.seen += 1;
            ctx.bytes += size;
            files.push(WalkedFile {
                path: entry.path(),
                rel,
                size,
            });
        }
    }
    Ok(())
}

/// Expand a ZIP archive into `dest` within the bounds.
///
/// Every entry name is component-checked (traversal, absolute paths,
/// backslash tricks reject the whole archive with the offending name in
/// the error); symlink/device/fifo entries reject the archive; declared
/// uncompressed sizes are checked against the byte bound up front and
/// actual streamed bytes re-checked, so a lying header cannot slip a
/// bomb through. On any violation nothing is kept: the caller drops
/// `dest` (an extraction temp dir), and this function removes anything
/// it already wrote before returning the error.
///
/// Returns the number of files extracted.
pub fn expand_zip_bounded(zip_path: &Path, dest: &Path, bounds: &ImportBounds) -> Result<usize> {
    let file = fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| Error::ImportBounds(format!("not a readable ZIP archive: {e}")))?;
    if archive.len() > bounds.max_files {
        return Err(Error::ImportBounds(format!(
            "ZIP holds {} entries; the import file-count bound is {}",
            archive.len(),
            bounds.max_files
        )));
    }
    // Declared-size preflight: uncompressed totals, not compressed.
    let mut declared_total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| Error::ImportBounds(format!("ZIP entry {i} unreadable: {e}")))?;
        declared_total = declared_total.saturating_add(entry.size());
        if declared_total > bounds.max_total_bytes {
            return Err(Error::ImportBounds(format!(
                "ZIP declares {} bytes uncompressed; the import byte bound is {}",
                declared_total, bounds.max_total_bytes
            )));
        }
    }
    let mut written = Vec::new();
    let mut extracted = 0usize;
    let expand = (|| -> Result<()> {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| Error::ImportBounds(format!("ZIP entry {i} unreadable: {e}")))?;
            let raw_name = entry
                .name()
                .map_err(|e| {
                    Error::ImportBounds(format!("ZIP entry {i} has an unusable name: {e}"))
                })?
                .to_string();
            let Some(target) = zip_entry_target(dest, &raw_name, bounds) else {
                return Err(Error::ImportBounds(format!(
                    "ZIP entry “{raw_name}” escapes the extraction root (traversal or absolute path)"
                )));
            };
            if entry.is_dir() {
                fs::create_dir_all(&target)?;
                continue;
            }
            // Non-regular UNIX entry types (symlink, fifo, device)
            // never materialize as files.
            if let Some(mode) = entry.unix_mode() {
                let kind = mode & 0o170000;
                if kind == 0o120000 {
                    return Err(Error::ImportBounds(format!(
                        "ZIP entry “{raw_name}” is a symlink; symlink entries are not extracted"
                    )));
                }
                if kind != 0o100000 && kind != 0 {
                    return Err(Error::ImportBounds(format!(
                        "ZIP entry “{raw_name}” is not a regular file (mode {mode:o}); refused"
                    )));
                }
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&target)?;
            let mut written_bytes: u64 = 0;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = entry.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                written_bytes += n as u64;
                if written_bytes > entry.size() {
                    return Err(Error::ImportBounds(format!(
                        "ZIP entry “{raw_name}” streamed more bytes than its header declares"
                    )));
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
            }
            if written_bytes != entry.size() {
                return Err(Error::ImportBounds(format!(
                    "ZIP entry “{raw_name}” streamed {} bytes but declared {}",
                    written_bytes,
                    entry.size()
                )));
            }
            written.push(target);
            extracted += 1;
        }
        Ok(())
    })();
    match expand {
        Ok(()) => Ok(extracted),
        Err(e) => {
            // Refuse partially: nothing of a rejected archive stays.
            for path in &written {
                fs::remove_file(path).ok();
            }
            // Prune now-empty directories we created, best effort.
            prune_empty_dirs(dest);
            Err(e)
        }
    }
}

/// The extraction target of one ZIP entry, or `None` when the name
/// would escape `dest`: every component must be plain, nesting within
/// the depth bound, and the name is rejected when it carries an
/// absolute or backslash-traversal shape.
fn zip_entry_target(dest: &Path, raw_name: &str, bounds: &ImportBounds) -> Option<PathBuf> {
    let normalized = raw_name.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return None; // leading `/` (or `\\\\?\`) — absolute entries are refused
    }
    let mut target = dest.to_path_buf();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                depth += 1;
                if depth > bounds.max_depth {
                    return None;
                }
                target.push(part);
            }
            _ => return None, // `..`, `.`, root, prefix
        }
    }
    Some(target)
}

/// Remove `dir`'s now-empty subdirectories (best effort; used to undo
/// a partial extraction). `dir` itself is never removed — it belongs
/// to the caller (an extraction temp dir) — and nothing that still
/// holds a file is touched.
fn prune_empty_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            prune_empty_dirs(&entry.path());
            let _ = fs::remove_dir(entry.path()); // fails when non-empty: exactly right
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mcv-bounds-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_child_refuses_traversal_and_outside_symlinks() {
        let dir = tmpdir("safe-child");
        let sub = dir.join("photos");
        fs::create_dir(&sub).unwrap();
        let file = sub.join("p.jpg");
        fs::write(&file, b"x").unwrap();

        assert_eq!(safe_child(&dir, "photos/p.jpg"), Some(file.clone()));
        assert_eq!(safe_child(&dir, "photos/missing.jpg"), None);
        assert_eq!(safe_child(&dir, "../escape.jpg"), None);
        assert_eq!(safe_child(&dir, "photos/../../escape.jpg"), None);
        assert_eq!(safe_child(&dir, "/etc/passwd"), None);
        assert_eq!(safe_child(&dir, "."), None);
        assert_eq!(safe_child(&dir, "..\\escape.jpg"), None);

        #[cfg(unix)]
        {
            let outside = dir.parent().unwrap().join("mcv-bounds-outside.txt");
            fs::write(&outside, b"out").unwrap();
            let link = dir.join("leak.txt");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert_eq!(safe_child(&dir, "leak.txt"), None);
            let _ = fs::remove_file(&outside);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_bounded_skips_hidden_dirs_and_follows_no_symlink() {
        let dir = tmpdir("walk");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        let media = dir.join("photos");
        fs::create_dir(&media).unwrap();
        fs::write(media.join("p.jpg"), b"photo").unwrap();
        fs::create_dir(dir.join(".git")).unwrap();
        fs::write(dir.join(".git/config"), b"cfg").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(media.join("p.jpg"), dir.join("p-link.jpg")).unwrap();
        }

        let walked = walk_bounded(&dir, &ImportBounds::default()).unwrap();
        let mut rels: Vec<&str> = walked.iter().map(|f| f.rel.as_str()).collect();
        rels.sort_unstable(); // directory order is fs-dependent
        assert_eq!(rels, vec!["a.txt", "photos/p.jpg"], "{rels:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_entry_target_refuses_traversal_shapes() {
        let bounds = ImportBounds::default();
        let dest = Path::new("/tmp/x");
        assert!(zip_entry_target(dest, "ok/file.txt", &bounds).is_some());
        assert!(zip_entry_target(dest, "../up.txt", &bounds).is_none());
        assert!(zip_entry_target(dest, "a/../../up.txt", &bounds).is_none());
        assert!(zip_entry_target(dest, "/abs.txt", &bounds).is_none());
        assert!(zip_entry_target(dest, "..\\up.txt", &bounds).is_none());
        // Nesting beyond the depth bound is refused.
        let deep = format!("{}{}file.txt", "d/".repeat(bounds.max_depth), "e/");
        assert!(zip_entry_target(dest, &deep, &bounds).is_none());
        assert!(zip_entry_target(dest, "a/b/file.txt", &bounds).is_some());
    }
}
