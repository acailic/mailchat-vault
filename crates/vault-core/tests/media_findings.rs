// Media integrity finding resolution: the detail half of the
// reconciliation surface. `media_reconcile` names damaged digests; this
// suite proves the resolution that makes a finding actionable:
//
// 1. a fixture with one deleted, one altered, and one unreferenced
//    object classifies into the right categories, and each missing or
//    corrupt digest resolves to the identities of the messages it
//    affects;
// 2. a digest shared by several messages resolves to every affected
//    message, in bounded pages with a correct total;
// 3. a missing registered unlinked file resolves to its import
//    provenance instead of an anonymous message failure;
// 4. resolution is a pure read — store bytes and the archive revision
//    are unchanged — and re-running reflects the current store state.

use std::fs;
use std::path::{Path, PathBuf};

use vault_core::chat_export::import_bounded;
use vault_core::media::store as store_media;
use vault_core::object_store::{Capacity, ObjectStore};
use vault_core::storage::Storage;

/// Real small files committed with the repo (the same fixtures the
/// object-store suite uses).
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
    #[allow(dead_code)] // dropped when the sandbox goes out of scope
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

/// A snapshot of every file under `media/`: path + bytes. Resolution
/// must leave this exactly unchanged (read-only, diagnostic).
fn store_snapshot(sb: &Sandbox) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&sb.data_dir().join("media"), &mut out);
    out
}

/// The unreferenced export file `damaged_import` plants: stored by the
/// unlinked pass, registered with its provenance, referenced by no
/// message.
const STRAY_NAME: &str = "stray-map.png";
const STRAY_BYTES: &[u8] = b"stray export bytes that no message references";

/// A Telegram export tree with three referenced media files plus one
/// unreferenced stray file (stored and registered by the whole-tree
/// import's unlinked pass). Returns the run's import id.
fn damaged_import(sb: &Sandbox, storage: &mut Storage) -> String {
    sb.put("photos/photo_102@01-11-2023_11-02-00.jpg", "photo.jpg");
    sb.put("files/plan.pdf", "plan.pdf");
    sb.put("voice_messages/audio_104@01-11-2023.ogg", "beep.wav");
    fs::write(sb.export_dir().join(STRAY_NAME), STRAY_BYTES).unwrap();
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
    let outcome = import_bounded(
        storage,
        &sb.export_dir(),
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 3, "{:?}", outcome.summary);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.stored, 3, "{media:?}"); // the referenced files
    assert_eq!(media.unlinked, 1, "{media:?}"); // the stray file
    outcome.import_id
}

#[test]
fn findings_classify_and_resolve_to_affected_messages() {
    let sb = Sandbox::new("findings-classify");
    let mut storage = Storage::open_in_memory().unwrap();
    damaged_import(&sb, &mut storage);
    let store = sb.store();

    // Damage: delete the photo's object (missing), alter the plan's
    // object (corrupt), and add an unreferenced object directly to the
    // store (orphan — registered by no message and no registry row).
    let photo_sha = sha256_hex(&fixture("photo.jpg"));
    let plan_sha = sha256_hex(&fixture("plan.pdf"));
    fs::remove_file(
        store
            .store_dir()
            .join(store.object_name(&photo_sha).unwrap()),
    )
    .unwrap();
    let plan_path = store
        .store_dir()
        .join(store.object_name(&plan_sha).unwrap());
    let mut bytes = fs::read(&plan_path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&plan_path, bytes).unwrap();
    let extra = sb.root.join("extra.bin");
    fs::write(&extra, b"unreferenced bytes").unwrap();
    let (extra_entry, _) = store_media(
        &sb.data_dir(),
        &extra,
        "extra.bin",
        "document",
        &mut Capacity::default(),
    )
    .unwrap();
    let extra_sha = extra_entry.sha256.clone();

    // The classification: one missing, one corrupt, one orphan.
    let digests = storage.media_digests().unwrap();
    let report = store.reconcile(&digests, true).unwrap();
    assert_eq!(report.missing, vec![photo_sha.clone()], "{report:?}");
    assert_eq!(report.corrupt.len(), 1, "{report:?}");
    assert!(
        report.corrupt[0].starts_with(&store.object_name(&plan_sha).unwrap()),
        "{report:?}"
    );
    assert_eq!(report.orphans, vec![extra_sha.clone()], "{report:?}");

    // Missing resolves to exactly the message that referenced the photo:
    // identity (author) plus conversation/date context and the display
    // name the reference kept.
    let missing = storage.media_digest_references(&photo_sha, 0, 50).unwrap();
    assert_eq!(missing.total, 1);
    assert_eq!(missing.references.len(), 1);
    let reference = &missing.references[0];
    assert_eq!(reference.author, "Ana");
    assert_eq!(reference.conversation, "Arhiva ekipa");
    assert_eq!(reference.chat_id, "Arhiva ekipa");
    assert!(
        reference.name.contains("photo_102"),
        "kept the export display name: {}",
        reference.name
    );
    assert!(!reference.date.is_empty(), "the message's date is carried");

    // Corrupt resolves to the plan's message the same way.
    let corrupt = storage.media_digest_references(&plan_sha, 0, 50).unwrap();
    assert_eq!(corrupt.total, 1);
    assert_eq!(corrupt.references[0].author, "Marko");
    assert!(corrupt.references[0].name.contains("plan.pdf"));

    // The unreferenced object is no message failure: it resolves to an
    // empty page — it never had a message.
    let orphan = storage.media_digest_references(&extra_sha, 0, 50).unwrap();
    assert_eq!(orphan.total, 0);
    assert!(orphan.references.is_empty());

    // And a healthy digest resolves fine too (the voice message).
    let beep_sha = sha256_hex(&fixture("beep.wav"));
    let healthy = storage.media_digest_references(&beep_sha, 0, 50).unwrap();
    assert_eq!(healthy.total, 1);
    assert_eq!(healthy.references[0].author, "Jelena");
}

#[test]
fn a_shared_digest_resolves_to_every_affected_message_in_bounded_pages() {
    let sb = Sandbox::new("findings-shared");
    // Five messages referencing one shared photo: one object on disk
    // (dedup), five affected messages when it goes missing.
    sb.put("photos/shared.jpg", "photo.jpg");
    let authors = ["Ana", "Marko", "Jelena", "Petar", "Milica"];
    let messages: Vec<String> = authors
        .iter()
        .enumerate()
        .map(|(i, author)| {
            format!(
                r#"{{ "id": {}, "type": "message", "date_unixtime": "1698832920",
      "from": "{}", "photo": "photos/shared.jpg", "text": "" }}"#,
                200 + i,
                author
            )
        })
        .collect();
    fs::write(
        sb.export_dir().join("result.json"),
        format!(
            r#"{{"name": "Arhiva ekipa", "type": "private_group", "id": 1,
  "messages": [{}]}}"#,
            messages.join(",\n    ")
        ),
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    import_bounded(
        &mut storage,
        &sb.export_dir(),
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    let store = sb.store();
    let shared_sha = sha256_hex(&fixture("photo.jpg"));
    assert!(
        store.object_name(&shared_sha).is_some(),
        "one shared object on disk"
    );

    // Page 1 of 2, page 2 of 2, and past the end: every affected
    // message, in order, with a stable total.
    let mut seen: Vec<(i64, String)> = Vec::new();
    for offset in [0u32, 2, 4, 6] {
        let page = storage
            .media_digest_references(&shared_sha, offset, 2)
            .unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.offset, offset);
        assert_eq!(page.limit, 2);
        assert!(page.references.len() <= 2, "bounded page");
        seen.extend(
            page.references
                .iter()
                .map(|r| (r.message_id, r.author.clone())),
        );
    }
    assert_eq!(
        seen,
        [
            (1, "Ana".into()),
            (2, "Marko".into()),
            (3, "Jelena".into()),
            (4, "Petar".into()),
            (5, "Milica".into()),
        ],
        "every affected message, oldest id first"
    );

    // The count is honest against the store: deleting the shared object
    // makes the finding, and the same resolution drives it.
    fs::remove_file(
        store
            .store_dir()
            .join(store.object_name(&shared_sha).unwrap()),
    )
    .unwrap();
    let digests = storage.media_digests().unwrap();
    let report = store.reconcile(&digests, false).unwrap();
    assert_eq!(report.missing, vec![shared_sha.clone()], "{report:?}");
    assert_eq!(
        storage
            .media_digest_references(&shared_sha, 0, 50)
            .unwrap()
            .total,
        5
    );
}

#[test]
fn a_missing_registered_unlinked_file_resolves_to_its_import_provenance() {
    let sb = Sandbox::new("findings-unlinked");
    let mut storage = Storage::open_in_memory().unwrap();
    let import_id = damaged_import(&sb, &mut storage);
    let store = sb.store();

    // The stray file was stored and registered (its registry row is its
    // reference), so a clean import reconciles clean.
    let digests = storage.media_digests().unwrap();
    let clean = store.reconcile(&digests, true).unwrap();
    assert!(clean.is_clean(), "{clean:?}");

    // Delete the unlinked object: the finding is a missing digest with
    // no message behind it…
    let stray_sha = {
        let row = &storage.unlinked_media_objects().unwrap()[0];
        assert_eq!(row.name, STRAY_NAME);
        row.sha256.clone()
    };
    fs::remove_file(
        store
            .store_dir()
            .join(store.object_name(&stray_sha).unwrap()),
    )
    .unwrap();
    let report = store.reconcile(&digests, true).unwrap();
    assert_eq!(report.missing, vec![stray_sha.clone()], "{report:?}");
    let resolution = storage.media_digest_references(&stray_sha, 0, 50).unwrap();
    assert_eq!(resolution.total, 0, "no message references it");
    assert!(resolution.references.is_empty());

    // …and its known import — not an anonymous failure — is the registry
    // row the finding resolves through.
    let registered = storage.unlinked_media_objects().unwrap();
    assert_eq!(registered.len(), 1, "{registered:?}");
    let row = &registered[0];
    assert_eq!(row.origin, "unlinked");
    assert_eq!(row.import_id, import_id, "the import that stored it");
    assert_eq!(row.kind, "image");
    assert_eq!(row.size, STRAY_BYTES.len() as u64);
}

#[test]
fn resolution_is_read_only_and_reruns_reflect_the_current_store() {
    let sb = Sandbox::new("findings-readonly");
    let mut storage = Storage::open_in_memory().unwrap();
    damaged_import(&sb, &mut storage);
    let store = sb.store();
    let photo_sha = sha256_hex(&fixture("photo.jpg"));

    fs::remove_file(
        store
            .store_dir()
            .join(store.object_name(&photo_sha).unwrap()),
    )
    .unwrap();
    let digests = storage.media_digests().unwrap();
    let report = store.reconcile(&digests, true).unwrap();
    assert_eq!(report.missing, vec![photo_sha.clone()], "{report:?}");

    // Resolution (every page, plus the registry read) writes nothing:
    // store bytes and the archive revision are exactly unchanged.
    let before = store_snapshot(&sb);
    let revision_before = storage.archive_revision().unwrap();
    let mut offset = 0;
    loop {
        let page = storage
            .media_digest_references(&photo_sha, offset, 1)
            .unwrap();
        assert_eq!(page.total, 1);
        offset += 1;
        if page.references.is_empty() || offset > 10 {
            break;
        }
    }
    let _ = storage.unlinked_media_objects().unwrap();
    assert_eq!(store_snapshot(&sb), before, "no object was touched");
    assert_eq!(
        storage.archive_revision().unwrap(),
        revision_before,
        "no archive write happened"
    );

    // Restore the object from the original fixture bytes (the
    // documented recovery: re-import or restore a known-good backup):
    // re-running the check reflects the resolved state.
    let (entry, _) = store_media(
        &sb.data_dir(),
        &fixture("photo.jpg"),
        "photo.jpg",
        "image",
        &mut Capacity::default(),
    )
    .unwrap();
    assert_eq!(entry.sha256, photo_sha);
    let digests = storage.media_digests().unwrap();
    let report = store.reconcile(&digests, true).unwrap();
    assert!(report.missing.is_empty(), "{report:?}");
    assert!(
        report.corrupt.iter().all(|c| !c.contains(&photo_sha)),
        "{report:?}"
    );
}
