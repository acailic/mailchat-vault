//! M4.1: the content-addressed object store behind the vault's `media/`
//! directory.
//!
//! Objects are stored by SHA-256 digest — byte-exact copies of their
//! input (input-original preservation: contents are never transcoded,
//! re-encoded, or otherwise rewritten, and every reference keeps the
//! original display name, kind, and size) — so identical files (a
//! re-import, the same photo in two chats, the same bytes under two
//! names) are exactly one object on disk.
//!
//! # Lifecycle: stage → promote → reference
//!
//! Writes follow an explicit three-step lifecycle so no half-written
//! object is ever visible and no reference can dangle:
//!
//! 1. **stage** — bytes stream into [`ObjectStore::stage`] /
//!    [`ObjectStore::stage_reader`], which hashes exactly what it writes
//!    into `media/.staging/` (never the objects area) while enforcing
//!    the caller's [`Capacity`] budget.
//! 2. **promote** — [`ObjectStore::promote`] atomically renames the
//!    staged file to its final `media/<sha256>.<ext>` name (`rename(2)`
//!    is atomic: readers see either the whole object or none of it) or,
//!    when identical content is already stored under any extension,
//!    drops the staging file and reuses the existing object.
//! 3. **reference** — only after promote returns does the caller record
//!    the digest (the `raw_json` `media` array, written in the ingest
//!    batch commit).
//!
//! Crash at any point leaves a sound state: before promote, only staging
//! litter exists (invisible to enumeration, swept by
//! [`ObjectStore::sweep_staging`]); during promote, the rename is
//! atomic; between promote and reference, the object is a harmless
//! orphan the next import of the same content dedups onto — a dangling
//! reference (digest recorded, object absent) can never be produced by
//! this order.
//!
//! # Reconciliation
//!
//! [`ObjectStore::reconcile`] is the integrity tool: given the digests
//! the archive references (see [`crate::storage::Storage::media_digests`]),
//! it verifies every reference has an object and reports what the store
//! holds beyond those references — orphans, corrupt objects (content
//! hash ≠ name digest, when `verify` is set), staging litter, and
//! foreign files. A clean import reconciles to zero orphans, zero
//! missing, zero corrupt.

use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Per-process staging-name sequence: two stages in one process can
/// never collide on a name, whatever the clock says.
static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A staging-area / capacity budget for one import run: how many bytes
/// and objects the run may **add** to the store. Deduplicated content
/// (an object already in the store) is refunded, so re-importing an
/// archive whose media is already stored never eats into the budget.
#[derive(Debug, Clone)]
pub struct Capacity {
    /// Most new object bytes one import may store.
    pub max_bytes: u64,
    /// Most new objects one import may store.
    pub max_objects: u64,
    used_bytes: u64,
    used_objects: u64,
}

impl Capacity {
    /// The default media budget for a chat-export import: generous for
    /// real personal archives (a WhatsApp export with media, a Telegram
    /// export with documents), while bounding a runaway or malicious
    /// input. Kept in step with [`crate::import_bounds::ImportBounds`].
    pub const DEFAULT_MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024; // 32 GiB
    /// Default object-count bound for one import.
    pub const DEFAULT_MAX_OBJECTS: u64 = 100_000;

    pub fn new(max_bytes: u64, max_objects: u64) -> Self {
        Self {
            max_bytes,
            max_objects,
            used_bytes: 0,
            used_objects: 0,
        }
    }

    /// How much of the budget this run has consumed so far:
    /// `(bytes, objects)`.
    pub fn used(&self) -> (u64, u64) {
        (self.used_bytes, self.used_objects)
    }

    /// Charge room for one prospective object (`size` bytes). Staging
    /// is charged provisionally — the guard must hold even before the
    /// content is known to be a duplicate — and [`Capacity::refund`]
    /// gives the room back when promote turns out to be a dedup hit.
    fn charge(&mut self, size: u64) -> Result<()> {
        if self.used_objects + 1 > self.max_objects {
            return Err(Error::ImportBounds(format!(
                "media import exceeds the object budget: {} objects (limit {})",
                self.used_objects + 1,
                self.max_objects
            )));
        }
        if self.used_bytes + size > self.max_bytes {
            return Err(Error::ImportBounds(format!(
                "media import exceeds the byte budget: {} new bytes > limit {}",
                self.used_bytes + size,
                self.max_bytes
            )));
        }
        self.used_objects += 1;
        self.used_bytes += size;
        Ok(())
    }

    /// Charge `extra` bytes discovered while streaming (a source whose
    /// size was not known up front, or grew past its up-front charge).
    fn charge_growth(&mut self, extra: u64, total_now: u64) -> Result<()> {
        if self.used_bytes + extra > self.max_bytes {
            return Err(Error::ImportBounds(format!(
                "media import exceeds the byte budget while streaming: {} bytes > limit {}",
                total_now, self.max_bytes
            )));
        }
        self.used_bytes += extra;
        Ok(())
    }

    /// Give budget room back: promote found identical content already
    /// stored, so this run added nothing for this object.
    fn refund(&mut self, size: u64) {
        self.used_bytes = self.used_bytes.saturating_sub(size);
        self.used_objects = self.used_objects.saturating_sub(1);
    }
}

impl Default for Capacity {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_BYTES, Self::DEFAULT_MAX_OBJECTS)
    }
}

/// One object present in the store (a file named `<sha256>.<ext>`
/// directly under the store directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreObject {
    pub sha256: String,
    /// Store-relative object path, `media/<sha256>.<ext>`.
    pub path: String,
    pub size: u64,
}

/// A staged-but-not-yet-promoted object: the digest is known, the bytes
/// sit in the staging area, and nothing visible references them. Drop
/// it without promoting to simulate (or recover from) an interruption
/// before promote — the staging file is removed on drop, matching what
/// [`ObjectStore::sweep_staging`] does after a real crash.
#[derive(Debug)]
pub struct StagedObject {
    staging_path: PathBuf,
    pub sha256: String,
    pub size: u64,
    #[allow(dead_code)] // carried for debugging; the ext is baked into the path
    ext: String,
}

impl Drop for StagedObject {
    fn drop(&mut self) {
        // An abandoned staging file is invisible to enumeration either
        // way, but remove it eagerly when we can — Drop cannot run at
        // all after a hard crash, which is exactly what sweep_staging
        // covers. (Budget refunds are the wrappers' concern; by the
        // time a StagedObject is dropped, promote has either refunded
        // it or the run is failing.)
        fs::remove_file(&self.staging_path).ok();
    }
}

/// A promoted object: content-addressed, visible in the store, ready to
/// be referenced. Recording the digest in the archive is the caller's
/// step (the ingest batch commit) — see the module lifecycle notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub sha256: String,
    /// Store-relative object path, `media/<sha256>.<ext>`.
    pub path: String,
    pub size: u64,
    /// Whether this promote wrote the object (`false` = identical
    /// content was already stored and was reused).
    pub written: bool,
}

/// The reconciliation report: everything the store holds, checked
/// against everything the archive references.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Objects in the store (valid `<sha256>.<ext>` names).
    pub objects: u64,
    /// Distinct digests the archive references.
    pub referenced: usize,
    /// Referenced digests with no object in the store (dangling
    /// references — the state the stage→promote→reference order makes
    /// unreachable by construction; their presence means external
    /// damage, e.g. a deleted media file).
    pub missing: Vec<String>,
    /// Objects nothing references (stored but unreferenced — harmless;
    /// a later import of the same content dedups onto them).
    pub orphans: Vec<String>,
    /// Objects whose contents hash to something other than their name
    /// digest (corruption; only checked with `verify`).
    pub corrupt: Vec<String>,
    /// Staging-area leftovers (interrupted writes) plus legacy
    /// `.tmp-*` files from older builds.
    pub stale_staging: Vec<String>,
    /// Files in the store directory that are neither objects nor
    /// staging litter (foreign names).
    pub foreign: Vec<String>,
}

impl ReconcileReport {
    /// The store is exactly consistent with the references: every
    /// reference has its object, every object is referenced, nothing is
    /// corrupt, and no litter or foreign files sit in the store.
    pub fn is_clean(&self) -> bool {
        self.objects as usize == self.referenced
            && self.missing.is_empty()
            && self.orphans.is_empty()
            && self.corrupt.is_empty()
            && self.stale_staging.is_empty()
            && self.foreign.is_empty()
    }
}

/// The content-addressed object store rooted at `<data_dir>/media/`.
///
/// A cheap value type: construct per operation (as the media pass does
/// per file) or keep for a run; it holds no locks and no state beyond
/// the two paths.
#[derive(Debug, Clone)]
pub struct ObjectStore {
    store_dir: PathBuf,
    staging_dir: PathBuf,
}

impl ObjectStore {
    /// The store for the vault at `data_dir` (objects directly under
    /// `media/`, staging under `media/.staging/`).
    pub fn new(data_dir: &Path) -> Self {
        let store_dir = data_dir.join("media");
        let staging_dir = store_dir.join(".staging");
        Self {
            store_dir,
            staging_dir,
        }
    }

    /// The objects directory (`<data_dir>/media`).
    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    /// Stage a file from disk: the source's size is charged up front,
    /// then bytes stream into the staging area while hashing them — the
    /// digest describes exactly the staged bytes, never the source, and
    /// a source that grows mid-copy is caught per chunk.
    pub fn stage(&self, src: &Path, ext: &str, budget: &mut Capacity) -> Result<StagedObject> {
        let file = fs::File::open(src)?; // before charging: nothing to refund on open failure
        let size = fs::metadata(src)?.len();
        budget.charge(size)?;
        match self.stage_impl(file, ext, budget, size) {
            Ok(staged) => {
                // The source shrank under the up-front charge (a file
                // replaced mid-import): refund the phantom excess so
                // the budget reflects the bytes that actually landed.
                budget.refund_bytes_only(size.saturating_sub(staged.size));
                Ok(staged)
            }
            Err(e) => {
                // stage_impl refunded only its growth; refund the
                // up-front bytes and the object count.
                budget.refund(size);
                Err(e)
            }
        }
    }

    /// Stage bytes from any reader (e.g. a ZIP entry stream), where the
    /// size is unknown or untrusted: every chunk is budget-checked as
    /// it flows, so a lying declared size cannot blow past the budget.
    pub fn stage_reader(
        &self,
        reader: impl Read,
        ext: &str,
        budget: &mut Capacity,
    ) -> Result<StagedObject> {
        budget.charge(0)?;
        match self.stage_impl(reader, ext, budget, 0) {
            Ok(staged) => Ok(staged),
            Err(e) => {
                // stage_impl refunded its streamed bytes; refund the
                // object count.
                budget.refund(0);
                Err(e)
            }
        }
    }

    /// Shared staging body. `charged_up_front` bytes are already
    /// charged (`0` means per-chunk charging). On success the object's
    /// whole size is charged (either up front or chunk by chunk); on
    /// failure the partial staging file is removed and only what
    /// **this** body charged beyond `charged_up_front` is refunded —
    /// the wrapper refunds the up-front part.
    fn stage_impl(
        &self,
        mut reader: impl Read,
        ext: &str,
        budget: &mut Capacity,
        charged_up_front: u64,
    ) -> Result<StagedObject> {
        use sha2::{Digest, Sha256};
        fs::create_dir_all(&self.staging_dir)?;
        let staging_path = self.staging_dir.join(format!(
            "{}-{}-{}.{}",
            std::process::id(),
            STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            sanitize_ext(ext)
        ));
        let mut out = fs::File::create(&staging_path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size: u64 = 0;
        let mut growth_charged: u64 = 0; // bytes charged here, beyond the up-front amount
        let result = loop {
            match reader.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    let new_size = size + n as u64;
                    // What this chunk adds beyond what is already
                    // charged: growth past the up-front charge (known
                    // size), or the whole chunk (unknown size).
                    let delta = if charged_up_front > 0 {
                        (new_size > charged_up_front + growth_charged)
                            .then(|| new_size - charged_up_front - growth_charged)
                    } else {
                        Some(n as u64)
                    };
                    if let Some(delta) = delta {
                        if let Err(e) = budget.charge_growth(delta, new_size) {
                            break Err(e);
                        }
                        growth_charged += delta;
                    }
                    use std::io::Write;
                    if let Err(e) = out.write_all(&buf[..n]) {
                        break Err(e.into());
                    }
                    hasher.update(&buf[..n]);
                    size = new_size;
                }
                Err(e) => break Err(e.into()),
            }
        };
        match result {
            Ok(()) => {
                // Invariant: size == charged_up_front + growth_charged
                // (known size) or size == growth_charged (per-chunk).
                Ok(StagedObject {
                    staging_path,
                    sha256: format!("{:x}", hasher.finalize()),
                    size,
                    ext: sanitize_ext(ext),
                })
            }
            Err(e) => {
                fs::remove_file(&staging_path).ok();
                budget.refund_bytes_only(growth_charged);
                Err(e)
            }
        }
    }

    /// Promote a staged object: atomically rename it into the store at
    /// its content-addressed name, or — when identical content is
    /// already stored under this or another extension — drop the
    /// staging file, refund the budget, and reuse the existing object.
    pub fn promote(&self, staged: StagedObject, budget: &mut Capacity) -> Result<ObjectRef> {
        fs::create_dir_all(&self.store_dir)?;
        // `StagedObject` implements Drop (an abandoned staging file
        // cleans up after itself), so its fields are read, not moved.
        let sha256 = staged.sha256.clone();
        let size = staged.size;
        let exact_name = format!("{}.{}", staged.sha256, staged.ext.clone());
        let existing = if self.store_dir.join(&exact_name).exists() {
            Some(exact_name.clone())
        } else {
            existing_store_name(&self.store_dir, &sha256)
        };
        let is_dedup = existing.is_some();
        let file_name = match existing {
            Some(name) => {
                fs::remove_file(&staged.staging_path).ok();
                budget.refund(staged.size);
                name
            }
            None => {
                let target = self.store_dir.join(&exact_name);
                let mut written = true;
                if let Err(e) = fs::rename(&staged.staging_path, &target) {
                    // Another writer may have promoted the same content
                    // meanwhile; an atomic rename losing that race is a
                    // dedup hit. A collision that is not the same
                    // content (cryptographically impossible in
                    // practice) fails loudly.
                    let same_content = matches!(hash_file(&target), Ok((h, _)) if h == sha256);
                    fs::remove_file(&staged.staging_path).ok();
                    if !same_content {
                        return Err(e.into());
                    }
                    written = false;
                }
                if !written {
                    budget.refund(staged.size);
                }
                exact_name
            }
        };
        // Best effort: drop the staging directory when this was its
        // last file, so a quiescent store holds only objects.
        fs::remove_dir(&self.staging_dir).ok();
        Ok(ObjectRef {
            sha256,
            path: format!("media/{file_name}"),
            size,
            written: !is_dedup,
        })
    }

    /// Whether an object with this digest is in the store.
    pub fn has(&self, sha256: &str) -> bool {
        existing_store_name(&self.store_dir, sha256).is_some()
    }

    /// Resolve a store-relative reference (`media/<sha256>.<ext>`, the
    /// `path` recorded in a message's `media` entries) to its absolute
    /// path, refusing anything that is not exactly one store object:
    /// no traversal, no sibling names, no missing file. This is the
    /// one path the "open media" surface accepts — a stored digest
    /// name, never an arbitrary filesystem path.
    pub fn resolve_object(&self, store_rel: &str) -> Result<PathBuf> {
        let Some(rest) = store_rel.strip_prefix("media/") else {
            return Err(Error::Media(format!(
                "not a media store reference: {store_rel}"
            )));
        };
        let (stem, ext) = rest
            .rsplit_once('.')
            .ok_or_else(|| Error::Media(format!("not a media object name: {store_rel}")))?;
        if rest.contains('/')
            || rest.contains('\\')
            || sanitize_ext(ext) != ext.to_ascii_lowercase()
        {
            return Err(Error::Media(format!(
                "not a media object name: {store_rel}"
            )));
        }
        if stem.len() != 64 || !stem.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Media(format!(
                "not a media object name: {store_rel}"
            )));
        }
        let path = self
            .store_dir
            .join(format!("{stem}.{}", ext.to_ascii_lowercase()));
        if !path.is_file() {
            return Err(Error::Media(format!(
                "object not in the store: {store_rel}"
            )));
        }
        Ok(path)
    }

    /// Enumerate the visible objects: files named `<sha256>.<ext>`
    /// directly under the store directory. The staging directory and
    /// legacy `.tmp-*` files are never included — staged-but-unpromoted
    /// bytes are invisible by construction.
    pub fn enumerate(&self) -> Vec<StoreObject> {
        let mut objects = Vec::new();
        let Ok(entries) = fs::read_dir(&self.store_dir) else {
            return objects;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue; // `.staging/` or a stray directory
            }
            if name.starts_with(".tmp-") {
                continue; // legacy staging litter (reported by reconcile)
            }
            let Some(digest) = object_digest(&name) else {
                continue; // foreign file (reported by reconcile)
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            objects.push(StoreObject {
                sha256: digest,
                path: format!("media/{name}"),
                size: meta.len(),
            });
        }
        objects.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        objects
    }

    /// Remove staging litter: files under `media/.staging/` and legacy
    /// `.tmp-*` files in the store older than `age` (a fresh staging
    /// file may belong to an in-flight import; the default
    /// [`ObjectStore::SWEEP_AGE`] is far beyond any single copy).
    /// Returns the removed paths (store-relative).
    pub fn sweep_staging(&self, age: Duration) -> Vec<String> {
        let cutoff = SystemTime::now()
            .checked_sub(age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = Vec::new();
        let is_old = |entry: &fs::DirEntry| {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| t < cutoff)
                .unwrap_or(false)
        };
        // Legacy `.tmp-*` files directly in the store.
        if let Ok(entries) = fs::read_dir(&self.store_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(".tmp-")
                    && entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && is_old(&entry)
                    && fs::remove_file(entry.path()).is_ok()
                {
                    removed.push(format!("media/{name}"));
                }
            }
        }
        // Current staging area.
        if let Ok(entries) = fs::read_dir(&self.staging_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && is_old(&entry)
                    && fs::remove_file(entry.path()).is_ok()
                {
                    removed.push(format!("media/.staging/{name}"));
                }
            }
        }
        removed.sort();
        // Drop the staging directory itself when it is now empty.
        fs::remove_dir(&self.staging_dir).ok();
        removed
    }

    /// Default staging-sweep age: 24 h.
    pub const SWEEP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

    /// Reconcile the store against `referenced` — the set of digests
    /// the archive references ([`crate::storage::Storage::media_digests`]).
    ///
    /// Reports missing references, orphans, staging litter, and foreign
    /// files; with `verify`, every object's contents are re-hashed to
    /// confirm the name digest (O(store bytes) — run it from the
    /// maintenance surface, not per import).
    pub fn reconcile(&self, referenced: &HashSet<String>, verify: bool) -> Result<ReconcileReport> {
        let mut report = ReconcileReport {
            referenced: referenced.len(),
            ..ReconcileReport::default()
        };
        let mut on_disk: HashSet<String> = HashSet::new();
        let Ok(entries) = fs::read_dir(&self.store_dir) else {
            // No store at all: every reference is missing.
            report.missing = referenced.iter().cloned().collect();
            report.missing.sort();
            return Ok(report);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            if name == ".staging" {
                if let Ok(staged) = fs::read_dir(&self.staging_dir) {
                    for f in staged.flatten() {
                        if f.file_type().map(|t| t.is_file()).unwrap_or(false) {
                            report.stale_staging.push(format!(
                                "media/.staging/{}",
                                f.file_name().to_string_lossy()
                            ));
                        }
                    }
                }
                continue;
            }
            if !is_file {
                report.foreign.push(format!("media/{name}"));
                continue;
            }
            if name.starts_with(".tmp-") {
                report.stale_staging.push(format!("media/{name}"));
                continue;
            }
            let Some(digest) = object_digest(&name) else {
                report.foreign.push(format!("media/{name}"));
                continue;
            };
            report.objects += 1;
            on_disk.insert(digest.clone());
            if verify {
                let path = entry.path();
                match hash_file(&path) {
                    Ok((actual, _)) if actual == digest => {}
                    Ok((actual, _)) => report.corrupt.push(format!("{name} (hashes to {actual})")),
                    Err(_) => report.corrupt.push(format!("{name} (unreadable)")),
                }
            }
        }
        for digest in referenced {
            if !on_disk.contains(digest) {
                report.missing.push(digest.clone());
            }
        }
        report.missing.sort();
        for digest in &on_disk {
            if !referenced.contains(digest) {
                report.orphans.push(digest.clone());
            }
        }
        report.orphans.sort();
        report.stale_staging.sort();
        report.foreign.sort();
        Ok(report)
    }
}

impl Capacity {
    /// Refund streamed bytes only (the object count stays with the
    /// wrapper's refund).
    fn refund_bytes_only(&mut self, bytes: u64) {
        self.used_bytes = self.used_bytes.saturating_sub(bytes);
    }
}

/// The digest of an object file name (`<64-hex>.<ext>`), or `None` for
/// any other shape.
fn object_digest(name: &str) -> Option<String> {
    let (stem, _ext) = name.rsplit_once('.')?;
    let looks_hex = stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit());
    looks_hex.then(|| stem.to_ascii_lowercase())
}

/// Lowercase extension, kept when alphanumeric and reasonably short;
/// anything else (no extension, weird suffixes) becomes `bin`. Shared
/// with the media pass, which derives it from display names.
pub fn sanitize_ext(ext: &str) -> String {
    let ok = !ext.is_empty() && ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        ext.to_ascii_lowercase()
    } else {
        "bin".to_string()
    }
}

/// The name of an existing store file for this content, if any: the
/// first file whose name starts with the hash (the extension varies,
/// the hash does not).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_digest_parses_names() {
        let sha = "a".repeat(64);
        assert_eq!(object_digest(&format!("{sha}.jpg")), Some(sha.clone()));
        assert_eq!(object_digest(&format!("{sha}.BIN")), Some(sha.clone()));
        assert_eq!(object_digest(&format!("{}.jpg", "a".repeat(63))), None);
        assert_eq!(object_digest(&format!("{}z.jpg", "a".repeat(63))), None);
        assert_eq!(object_digest("notes.txt"), None);
        assert_eq!(object_digest(sha.as_str()), None);
    }

    #[test]
    fn sanitize_ext_matches_media_rules() {
        assert_eq!(sanitize_ext("JPG"), "jpg");
        assert_eq!(sanitize_ext(""), "bin");
        assert_eq!(sanitize_ext("a.very/long"), "bin");
    }
}

#[cfg(test)]
mod resolve_tests {
    //! M4.2: `resolve_object` — the one path the "open media" surface
    //! accepts: an exact store object name, never an arbitrary path.

    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mcv-resolve-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_a_stored_object_and_refuses_everything_else() {
        let dir = tmpdir("ok");
        let src = dir.join("photo.jpg");
        fs::write(&src, b"jpeg-bytes").unwrap();
        let data_root = dir.join("root");
        let object_store = ObjectStore::new(&data_root);
        let staged = object_store
            .stage(&src, "jpg", &mut Capacity::default())
            .unwrap();
        let object = object_store
            .promote(staged, &mut Capacity::default())
            .unwrap();

        let resolved = object_store.resolve_object(&object.path).unwrap();
        assert_eq!(
            resolved,
            data_root
                .join("media")
                .join(format!("{}.jpg", object.sha256))
        );
        assert!(resolved.is_file());

        // Traversal, foreign names, and absent objects are refused.
        let sha = "a".repeat(64);
        for bad in [
            "media/../../etc/passwd",
            &format!("media/{sha}.jpg"),
            &format!("media/{sha}"),
            &format!("{sha}.jpg"),
            "media/sub/../../x.jpg",
            "media/notepad.txt",
            "",
        ] {
            assert!(object_store.resolve_object(bad).is_err(), "{bad:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
