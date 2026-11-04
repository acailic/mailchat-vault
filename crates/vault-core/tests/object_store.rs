// M4.1 object-store contracts, proven against the real lifecycle:
//
// 1. interrupted writes at three points (before / during / after
//    promote) leave no dangling reference and no visible half-object;
// 2. two identical files are exactly one object on disk;
// 3. traversal (`../`), symlink, and over-capacity fixtures (directory
//    trees and ZIP archives) are rejected cleanly;
// 4. the reconciliation report shows zero orphans after a clean import
//    — and classifies every damaged state (missing / orphan / corrupt /
//    staging litter / foreign).

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use vault_core::chat_export::import_bounded;
use vault_core::import_bounds::{expand_zip_bounded, safe_child, walk_bounded, ImportBounds};
use vault_core::media::store as store_media;
use vault_core::object_store::{Capacity, ObjectStore, ReconcileReport};
use vault_core::storage::Storage;

/// Real small files committed with the repo (same fixtures the media
/// extraction suite uses).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).unwrap());
    format!("{:x}", hasher.finalize())
}

/// One temp workspace per test: an `export/` tree and a `data/` app-data
/// dir, so store writes never touch the repo.
struct Sandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn export_dir(&self) -> PathBuf {
        let dir = self.root.join("export");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn store(&self) -> ObjectStore {
        ObjectStore::new(&self.data_dir())
    }

    /// Copy a fixture into the export tree at `rel`.
    fn put(&self, rel: &str, fixture_name: &str) {
        let target = self.export_dir().join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(fixture(fixture_name), &target).unwrap();
    }
}

/// Every file directly under `media/` (objects, staging dir, litter).
fn store_entries(store: &ObjectStore) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(store.store_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn object_files(store: &ObjectStore) -> Vec<String> {
    store
        .enumerate()
        .iter()
        .map(|o| o.path.strip_prefix("media/").unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// AC 1: interrupted writes at three lifecycle points
// ---------------------------------------------------------------------------

#[test]
fn interruption_before_promote_leaves_no_visible_object() {
    let sb = Sandbox::new("crash-before-promote");
    let store = sb.store();
    let src = sb.export_dir().join("photo.jpg");
    fs::copy(fixture("photo.jpg"), &src).unwrap();
    let mut budget = Capacity::default();

    // Stage, then "crash": the staged handle is dropped without promote.
    let staged = store.stage(&src, "jpg", &mut budget).unwrap();
    let digest = staged.sha256.clone();
    drop(staged);

    // No object is visible — not under its final name, not at all.
    assert!(object_files(&store).is_empty(), "nothing promoted");
    assert!(
        !sb.data_dir().join(format!("media/{digest}.jpg")).exists(),
        "no half-written object under its final name"
    );
    // Drop cleaned the staging file eagerly; a hard crash (no Drop) is
    // covered below by the raw-litter case.
    let staging = fs::read_dir(sb.data_dir().join("media/.staging"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(staging, 0, "staging cleaned by Drop");

    // The reference side: nothing recorded, so reconciliation sees no
    // dangling reference and no orphan.
    let report = store.reconcile(&HashSet::new(), false).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.objects, 0);
}

#[test]
fn hard_crash_before_promote_leaves_invisible_litter_not_an_object() {
    let sb = Sandbox::new("hard-crash-staging");
    let store = sb.store();
    // A staging file that a hard crash left behind (Drop never ran).
    let staging_dir = sb.data_dir().join("media/.staging");
    fs::create_dir_all(&staging_dir).unwrap();
    fs::write(staging_dir.join("9999-123.jpg"), b"half-written").unwrap();
    // Legacy litter from older builds sat directly in the store.
    fs::create_dir_all(store.store_dir()).unwrap();
    fs::write(store.store_dir().join(".tmp-9999-1"), b"old").unwrap();

    // Invisible to enumeration; reported as litter; swept by age 0.
    assert!(object_files(&store).is_empty());
    let report = store.reconcile(&HashSet::new(), false).unwrap();
    assert_eq!(report.objects, 0);
    assert_eq!(report.stale_staging.len(), 2, "{report:?}");
    assert_eq!(report.stale_staging[0], "media/.staging/9999-123.jpg");
    assert_eq!(report.stale_staging[1], "media/.tmp-9999-1");
    let swept = store.sweep_staging(std::time::Duration::ZERO);
    assert_eq!(swept.len(), 2, "{swept:?}");
    assert!(store_entries(&store).is_empty(), "store empty after sweep");
}

#[test]
fn interruption_at_promote_is_atomic_dedup_or_whole_object() {
    let sb = Sandbox::new("crash-during-promote");
    let store = sb.store();
    let src = sb.export_dir().join("photo.jpg");
    fs::copy(fixture("photo.jpg"), &src).unwrap();
    let mut budget = Capacity::default();

    // First object lands whole.
    let first = store
        .promote(store.stage(&src, "jpg", &mut budget).unwrap(), &mut budget)
        .unwrap();
    assert!(first.written);

    // The "during" boundary: promote is a rename — either the whole
    // object or the staging file. The dedup path (identical content
    // already promoted, staging about to be renamed) must drop the
    // staging file and reuse the object: exactly one file on disk.
    let second = store
        .promote(store.stage(&src, "jpg", &mut budget).unwrap(), &mut budget)
        .unwrap();
    assert!(!second.written, "identical content dedups");
    assert_eq!(second.sha256, first.sha256);
    assert_eq!(second.path, first.path);
    assert_eq!(object_files(&store), vec![format!("{}.jpg", first.sha256)]);

    // A staging file renamed to a target that exists with the same
    // content (the lost-rename race) is a dedup hit, not an error.
    let third = store
        .promote(store.stage(&src, "jpg", &mut budget).unwrap(), &mut budget)
        .unwrap();
    assert!(!third.written);
    assert_eq!(object_files(&store).len(), 1);
}

#[test]
fn interruption_after_promote_before_reference_leaves_orphan_not_dangling() {
    let sb = Sandbox::new("crash-after-promote");
    let store = sb.store();
    let src = sb.export_dir().join("photo.jpg");
    fs::copy(fixture("photo.jpg"), &src).unwrap();
    let mut budget = Capacity::default();

    // Promote, then "crash" before any reference is written.
    let object = store
        .promote(store.stage(&src, "jpg", &mut budget).unwrap(), &mut budget)
        .unwrap();
    let digest = object.sha256.clone();

    // No reference exists, so nothing can dangle; the object is an
    // orphan — reported exactly as such.
    let report = store.reconcile(&HashSet::new(), false).unwrap();
    assert_eq!(report.missing, Vec::<String>::new(), "{report:?}");
    assert_eq!(report.orphans, vec![digest.clone()], "{report:?}");

    // The orphan is not waste: storing the same content again dedups
    // onto it (no second copy, budget refunded).
    let (entry, written) =
        store_media(&sb.data_dir(), &src, "photo.jpg", "image", &mut budget).unwrap();
    assert!(!written);
    assert_eq!(entry.sha256, digest);
    assert_eq!(object_files(&store).len(), 1);
    // And once the archive references it, reconciliation is clean.
    let digests = HashSet::from([digest]);
    let report = store.reconcile(&digests, false).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

// ---------------------------------------------------------------------------
// AC 2: identical files are one object
// ---------------------------------------------------------------------------

#[test]
fn two_identical_files_are_one_object_on_disk() {
    let sb = Sandbox::new("dedup");
    let store = sb.store();
    let one = sb.export_dir().join("a.jpg");
    let two = sb.export_dir().join("b.JPEG");
    fs::copy(fixture("photo.jpg"), &one).unwrap();
    fs::copy(fixture("photo.jpg"), &two).unwrap();
    let mut budget = Capacity::default();

    let (first, written_one) =
        store_media(&sb.data_dir(), &one, "a.jpg", "image", &mut budget).unwrap();
    let (second, written_two) =
        store_media(&sb.data_dir(), &two, "b.JPEG", "image", &mut budget).unwrap();
    assert!(written_one);
    assert!(!written_two, "second store is a dedup hit");
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.path, second.path, "one physical object");
    assert_eq!(second.name, "b.JPEG", "the reference keeps its own name");

    assert_eq!(
        store_entries(&store).len(),
        1,
        "exactly one file in the store"
    );
    assert_eq!(
        store_entries(&store),
        vec![format!("{}.jpg", first.sha256)],
        "the first extension wins"
    );
    // The dedup refunded the second charge: the budget sees one object.
    assert_eq!(budget.used(), (fs::read(&one).unwrap().len() as u64, 1));
}

// ---------------------------------------------------------------------------
// AC 3: traversal / symlink / capacity fixtures rejected cleanly
// ---------------------------------------------------------------------------

#[test]
fn traversal_references_never_leave_the_export_root() {
    let sb = Sandbox::new("traversal");
    let root = sb.export_dir();
    let outside = sb.root.join("outside.jpg");
    fs::copy(fixture("photo.jpg"), &outside).unwrap();

    // `..` and absolute references are refused lexically…
    assert!(safe_child(&root, "../outside.jpg").is_none());
    assert!(safe_child(&root, "photos/../../outside.jpg").is_none());
    assert!(safe_child(&root, "/etc/passwd").is_none());
    assert!(safe_child(&root, "..\\outside.jpg").is_none());
    assert!(safe_child(&root, "photos\\..\\..\\outside.jpg").is_none());
    // …and a real inside-path still resolves.
    let inside = root.join("photos/p.jpg");
    fs::create_dir_all(root.join("photos")).unwrap();
    fs::copy(fixture("photo.jpg"), &inside).unwrap();
    assert_eq!(
        safe_child(&root, "photos/p.jpg"),
        Some(inside.canonicalize().unwrap())
    );
}

#[test]
fn symlinks_never_enter_a_walk_or_a_reference() {
    let sb = Sandbox::new("symlinks");
    let root = sb.export_dir();
    let outside = sb.root.join("outside.pdf");
    fs::copy(fixture("plan.pdf"), &outside).unwrap();

    #[cfg(unix)]
    {
        // A symlinked file in the tree (target inside the root)…
        let real = root.join("real.jpg");
        fs::copy(fixture("photo.jpg"), &real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link.jpg")).unwrap();
        // …a symlinked directory…
        let subdir = root.join("photos");
        fs::create_dir_all(&subdir).unwrap();
        fs::copy(fixture("photo.jpg"), subdir.join("p2.jpg")).unwrap();
        std::os::unix::fs::symlink(&subdir, root.join("photos-link")).unwrap();
        // …and a symlink pointing outside the root.
        std::os::unix::fs::symlink(&outside, root.join("leak.pdf")).unwrap();

        // The walk skips every symlink (files and directories alike).
        let walked = walk_bounded(&root, &ImportBounds::default()).unwrap();
        let rels: Vec<&str> = walked.iter().map(|f| f.rel.as_str()).collect();
        assert!(rels.contains(&"real.jpg"));
        assert!(rels.contains(&"photos/p2.jpg"));
        assert!(!rels.iter().any(|r| r.starts_with("link")), "{rels:?}");
        assert!(!rels.iter().any(|r| r.contains("leak")), "{rels:?}");

        // A reference that would resolve through the outside-pointing
        // symlink is refused after canonicalization; one resolving
        // through an inside-pointing symlink lands on its canonical
        // target (still inside the root).
        assert!(safe_child(&root, "leak.pdf").is_none());
        assert_eq!(
            safe_child(&root, "photos-link/p2.jpg"),
            Some(subdir.join("p2.jpg").canonicalize().unwrap())
        );
    }
}

#[test]
fn oversized_media_is_refused_and_stages_nothing() {
    let sb = Sandbox::new("capacity");
    let store = sb.store();
    let src = sb.export_dir().join("photo.jpg");
    fs::copy(fixture("photo.jpg"), &src).unwrap();
    let photo_len = fs::metadata(&src).unwrap().len();

    // A budget too small for the file refuses before anything lands.
    let mut tiny = Capacity::new(photo_len - 1, 10);
    let err = store_media(&sb.data_dir(), &src, "photo.jpg", "image", &mut tiny).unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
    assert!(object_files(&store).is_empty(), "no object staged");
    assert!(
        !sb.data_dir().join("media/.staging").exists()
            || fs::read_dir(sb.data_dir().join("media/.staging"))
                .unwrap()
                .count()
                == 0,
        "no staging litter"
    );
    // The failed charge is refunded: the budget is still usable.
    assert_eq!(tiny.used(), (0, 0));

    // The object-count bound refuses the second object of a 1-object
    // budget, cleanly, keeping the first.
    let mut one_object = Capacity::default();
    store_media(&sb.data_dir(), &src, "photo.jpg", "image", &mut one_object).unwrap();
    let other = sb.export_dir().join("plan.pdf");
    fs::copy(fixture("plan.pdf"), &other).unwrap();
    // First store consumed the count; a dedup hit must NOT be refused
    // (it adds nothing)…
    let (_, written) = store_media(
        &sb.data_dir(),
        &src,
        "photo-again.jpg",
        "image",
        &mut one_object,
    )
    .unwrap();
    assert!(!written, "dedup works at the object limit");
    // …but a genuinely new object is.
    let mut zero_objects = Capacity::new(u64::MAX, 0);
    let err = store_media(
        &sb.data_dir(),
        &other,
        "plan.pdf",
        "document",
        &mut zero_objects,
    )
    .unwrap_err();
    assert!(err.to_string().contains("object budget"), "{err}");
    assert_eq!(object_files(&store).len(), 1);
}

#[test]
fn directory_trees_over_the_bounds_are_refused() {
    let sb = Sandbox::new("tree-bounds");
    let root = sb.export_dir();
    for i in 0..3 {
        sb.put(&format!("m{i}.jpg"), "photo.jpg");
    }
    // File-count bound.
    let err = walk_bounded(
        &root,
        &ImportBounds {
            max_files: 2,
            max_total_bytes: u64::MAX,
            max_depth: 6,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("file-count bound"), "{err}");
    // Byte bound.
    let err = walk_bounded(
        &root,
        &ImportBounds {
            max_files: 20,
            max_total_bytes: fs::metadata(fixture("photo.jpg")).unwrap().len() * 2,
            max_depth: 6,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("byte bound"), "{err}");
    // Depth bound.
    let deep = root.join("a/b/c/d/e/f/g/h");
    fs::create_dir_all(&deep).unwrap();
    fs::copy(fixture("photo.jpg"), deep.join("deep.jpg")).unwrap();
    let err = walk_bounded(&root, &ImportBounds::default()).unwrap_err();
    assert!(err.to_string().contains("depth bound"), "{err}");
}

/// Build a ZIP with the given entries: `(name, bytes)`.
fn build_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

/// CRC-32 (IEEE, reflected) for the hand-rolled writer below.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let lsb = crc & 1;
            crc >>= 1;
            if lsb == 1 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}

/// Hand-rolled minimal ZIP writer for **hostile fixtures** — traversal
/// names, absolute names, symlink UNIX modes. `zip`'s own writer
/// sanitizes entry names and masks mode bits (by design), so a security
/// test that wants those shapes must craft the archive bytes with a
/// different tool than the reader. STORED entries only, one member per
/// `(name, bytes, unix-mode)` triple; the central directory carries the
/// mode as external attributes with a Unix "version made by".
fn build_evil_zip(path: &Path, entries: &[(&str, &[u8], u32)]) {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, bytes, mode) in entries {
        let offset = out.len() as u32;
        let name_bytes = name.as_bytes();
        let crc = crc32(bytes);
        // Local file header.
        out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date (1980-01-01)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes()); // csize
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes()); // usize
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(bytes);
        // Central directory record.
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central.extend_from_slice(&((3 << 8) | 20u16).to_le_bytes()); // made by: Unix
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central.extend_from_slice(&0x21u16.to_le_bytes()); // mod date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&((*mode << 16).to_le_bytes())); // external attrs
        central.extend_from_slice(&offset.to_le_bytes()); // local header offset
        central.extend_from_slice(name_bytes);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    fs::write(path, out).unwrap();
}

fn whatsapp_txt() -> Vec<u8> {
    "[1.11.2026., 09:00:00] Marko: Zdravo!\n\
     [1.11.2026., 09:04:00] Marko: IMG-20261101-WA0001.jpg (file attached)\n"
        .as_bytes()
        .to_vec()
}

#[test]
fn zip_traversal_entries_reject_the_whole_archive() {
    let sb = Sandbox::new("zip-traversal");
    let zip_path = sb.root.join("evil.zip");
    build_evil_zip(
        &zip_path,
        &[
            ("ok.txt", b"legit", 0o100644),
            ("../escape.txt", b"gotcha", 0o100644),
        ],
    );
    let dest = tempfile::tempdir().unwrap();
    let err = expand_zip_bounded(&zip_path, dest.path(), &ImportBounds::default()).unwrap_err();
    assert!(err.to_string().contains("escape.txt"), "{err}");
    assert!(
        fs::read_dir(dest.path()).unwrap().count() == 0,
        "nothing of a rejected archive stays"
    );

    // Absolute and backslash-traversal shapes refuse too.
    build_evil_zip(&zip_path, &[("/abs/evil.txt", b"x", 0o100644)]);
    assert!(expand_zip_bounded(&zip_path, dest.path(), &ImportBounds::default()).is_err());
    build_evil_zip(&zip_path, &[("..\\evil.txt", b"x", 0o100644)]);
    assert!(expand_zip_bounded(&zip_path, dest.path(), &ImportBounds::default()).is_err());
    assert!(fs::read_dir(dest.path()).unwrap().count() == 0);
}

#[test]
fn zip_symlink_and_capacity_fixtures_are_refused() {
    let sb = Sandbox::new("zip-symlink-cap");
    let zip_path = sb.root.join("crafted.zip");
    let dest = tempfile::tempdir().unwrap();

    // A symlink entry: UNIX mode bits say S_IFLNK (hand-rolled bytes —
    // the zip writer masks modes, so the fixture is crafted directly).
    build_evil_zip(&zip_path, &[("leak.txt", b"/etc/passwd", 0o120644)]);
    let err = expand_zip_bounded(&zip_path, dest.path(), &ImportBounds::default()).unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    assert!(fs::read_dir(dest.path()).unwrap().count() == 0);

    // Declared-size capacity: an entry larger than the byte bound is
    // refused up front (declared sizes are honored, then re-checked
    // against actual streamed bytes).
    let big = vec![0u8; 64 * 1024];
    build_zip(&zip_path, &[("a.bin", &big), ("b.bin", &big)]);
    let err = expand_zip_bounded(
        &zip_path,
        dest.path(),
        &ImportBounds {
            max_files: 20,
            max_total_bytes: 64 * 1024u64,
            max_depth: 6,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("byte bound"), "{err}");
    assert!(fs::read_dir(dest.path()).unwrap().count() == 0);

    // Entry-count capacity.
    build_zip(
        &zip_path,
        &[("1.txt", b"a"), ("2.txt", b"b"), ("3.txt", b"c")],
    );
    let err = expand_zip_bounded(
        &zip_path,
        dest.path(),
        &ImportBounds {
            max_files: 2,
            max_total_bytes: u64::MAX,
            max_depth: 6,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("file-count bound"), "{err}");
    assert!(fs::read_dir(dest.path()).unwrap().count() == 0);
}

#[test]
fn a_zip_import_flows_through_the_bounds_end_to_end() {
    // A WhatsApp export ZIP (the .txt plus its media), picked as a
    // single file: expanded under the bounds, imported, media staged,
    // promoted, and referenced.
    let sb = Sandbox::new("zip-import");
    let photo = fs::read(fixture("photo.jpg")).unwrap();
    let zip_path = sb.root.join("WhatsApp Chat with Ana.zip");
    build_zip(
        &zip_path,
        &[
            ("WhatsApp Chat with Ana.txt", &whatsapp_txt()),
            ("IMG-20261101-WA0001.jpg", &photo),
        ],
    );
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &zip_path,
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 2, "{:?}", outcome.summary);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.stored, 1, "{media:?}");
    assert_eq!(media.missing, 0, "{media:?}");

    // The object is referenced by exactly the stored message.
    let digests = storage.media_digests().unwrap();
    let photo_sha = sha256_hex(&fixture("photo.jpg"));
    assert_eq!(digests, HashSet::from([photo_sha.clone()]));
    let store = sb.store();
    assert_eq!(
        object_files(&store),
        vec![format!("{photo_sha}.jpg")],
        "media extracted out of the expanded ZIP"
    );
    let report = store.reconcile(&digests, true).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn a_wrapped_telegram_zip_descends_its_single_subdirectory() {
    let sb = Sandbox::new("zip-wrapped");
    let photo = fs::read(fixture("photo.jpg")).unwrap();
    let result_json = r#"{"name": "Arhiva", "type": "private_group", "id": 7, "messages": [{"id": 1, "type": "message", "date_unixtime": "1698832800", "from": "Marko", "photo": "photos/photo_1.jpg", "text": ""}]}"#;
    let zip_path = sb.root.join("telegram.zip");
    build_zip(
        &zip_path,
        &[
            ("ChatExport_2026-11-04/result.json", result_json.as_bytes()),
            ("ChatExport_2026-11-04/photos/photo_1.jpg", &photo),
        ],
    );
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &zip_path,
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1);
    assert_eq!(outcome.media.as_ref().unwrap().stored, 1);
    let digests = storage.media_digests().unwrap();
    assert_eq!(digests.len(), 1);
    let report = sb.store().reconcile(&digests, true).unwrap();
    assert!(report.is_clean(), "{report:?}");
}

// ---------------------------------------------------------------------------
// AC 4: reconciliation after a clean import, and every damaged state
// ---------------------------------------------------------------------------

/// A Telegram export with three real media files, imported cleanly.
fn clean_import(sb: &Sandbox, storage: &mut Storage) -> HashSet<String> {
    sb.put("photos/photo_102@01-11-2023_11-02-00.jpg", "photo.jpg");
    sb.put("files/plan.pdf", "plan.pdf");
    sb.put("voice_messages/audio_104@01-11-2023.ogg", "beep.wav");
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{
  "name": "Arhiva ekipa", "type": "private_group", "id": 123456789,
  "messages": [
    { "id": 102, "type": "message", "date_unixtime": "1698832920",
      "from": "Ana", "photo": "photos/photo_102@01-11-2023_11-02-00.jpg", "text": "" },
    { "id": 103, "type": "message", "date_unixtime": "1698832980",
      "from": "Marko", "file": "files/plan.pdf", "text": "" },
    { "id": 104, "type": "message", "date_unixtime": "1698833040",
      "from": "Jelena", "media_type": "voice_message",
      "file": "voice_messages/audio_104@01-11-2023.ogg", "text": "" }
  ]
}"#,
    )
    .unwrap();
    import_bounded(
        storage,
        &sb.export_dir(),
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    storage.media_digests().unwrap()
}

#[test]
fn reconciliation_is_clean_after_a_clean_import() {
    let sb = Sandbox::new("reconcile-clean");
    let mut storage = Storage::open_in_memory().unwrap();
    let digests = clean_import(&sb, &mut storage);
    assert_eq!(digests.len(), 3, "three distinct digests referenced");

    // Full verification: every reference has its object, every object
    // is referenced, every object hashes to its name — zero orphans.
    let report = sb.store().reconcile(&digests, true).unwrap();
    assert_eq!(report.objects, 3);
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.referenced, 3);
    assert_eq!(report.missing, Vec::<String>::new());
    assert_eq!(report.orphans, Vec::<String>::new());
    assert_eq!(report.corrupt, Vec::<String>::new());

    // Re-importing dedups onto the same objects — still clean.
    let digests_again = clean_import(&sb, &mut storage);
    assert_eq!(digests_again, digests);
    let report = sb.store().reconcile(&digests, true).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.objects, 3);
}

#[test]
fn reconciliation_classifies_every_damaged_state() {
    let sb = Sandbox::new("reconcile-damage");
    let mut storage = Storage::open_in_memory().unwrap();
    let digests = clean_import(&sb, &mut storage);
    let store = sb.store();
    let objects = store.enumerate();
    assert_eq!(objects.len(), 3);

    // 1. A deleted object → its reference dangles (external damage;
    //    the lifecycle itself cannot produce this).
    let victim = objects[0].clone();
    fs::remove_file(
        store
            .store_dir()
            .join(victim.path.strip_prefix("media/").unwrap()),
    )
    .unwrap();
    let mut report = store.reconcile(&digests, false).unwrap();
    assert_eq!(report.missing, vec![victim.sha256.clone()], "{report:?}");
    assert_eq!(report.orphans.len(), 0);
    assert!(!report.is_clean());

    // 2. An unreferenced but validly-named object → orphan.
    let extra = sb.root.join("extra.bin");
    fs::write(&extra, b"unreferenced bytes").unwrap();
    let mut budget = Capacity::default();
    store_media(&sb.data_dir(), &extra, "extra.bin", "document", &mut budget).unwrap();
    report = store.reconcile(&digests, false).unwrap();
    assert_eq!(report.orphans.len(), 1, "{report:?}");
    assert_eq!(report.objects, 3); // victim removed, extra added

    // 3. A corrupted object (content no longer hashes to its name).
    let corrupt = objects[1].clone();
    let path = store
        .store_dir()
        .join(corrupt.path.strip_prefix("media/").unwrap());
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    // Without verify, corruption is invisible (fast report)…
    report = store.reconcile(&digests, false).unwrap();
    assert!(report.corrupt.is_empty());
    // …with verify, it is named.
    report = store.reconcile(&digests, true).unwrap();
    let listed = report
        .corrupt
        .iter()
        .map(|c| c.split(' ').next().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        vec![corrupt.path.strip_prefix("media/").unwrap().to_string()],
        "{report:?}"
    );

    // 4. Foreign files never masquerade as objects.
    fs::write(store.store_dir().join("notes.txt"), b"hello").unwrap();
    report = store.reconcile(&digests, false).unwrap();
    assert!(
        report.foreign.contains(&"media/notes.txt".to_string()),
        "{report:?}"
    );

    // 5. Staging litter from an interrupted write is reported and swept.
    fs::create_dir_all(sb.data_dir().join("media/.staging")).unwrap();
    fs::write(sb.data_dir().join("media/.staging/1-1.jpg"), b"half").unwrap();
    report = store.reconcile(&digests, false).unwrap();
    assert_eq!(
        report.stale_staging,
        vec!["media/.staging/1-1.jpg".to_string()]
    );
    store.sweep_staging(std::time::Duration::ZERO);
    let report: ReconcileReport = store.reconcile(&digests, false).unwrap();
    assert!(report.stale_staging.is_empty());
}

#[test]
fn missing_store_reports_every_reference_missing() {
    let sb = Sandbox::new("reconcile-no-store");
    let digests = HashSet::from(["a".repeat(64), "b".repeat(64)]);
    let report = sb.store().reconcile(&digests, false).unwrap();
    assert_eq!(report.objects, 0);
    assert_eq!(report.missing.len(), 2);
    assert_eq!(report.orphans.len(), 0);
}
