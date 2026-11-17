//! A3b original retention for mail file imports: the opt-in flag makes
//! the importer store each message's raw RFC 5322 bytes through the
//! object store (stage → promote → reference, deduplicated by digest)
//! before the normalized record commits; an import without the flag —
//! the default — writes no rows and its records stay honestly labeled
//! "original not retained".
//!
//! Acceptance mapping (task A3b):
//! - `retention_off_by_default_writes_no_rows_and_labels_records`
//! - `retained_import_stores_exact_raw_bytes_that_survive_source_deletion`
//! - `reimport_with_retention_adds_zero_new_original_objects`
//! - `retention_on_a_reimport_links_originals_to_existing_rows`

use std::fs;
use std::path::{Path, PathBuf};

use vault_core::object_store::ObjectStore;
use vault_core::storage::Storage;
use vault_core::{mbox, ORIGINAL_NOT_RETAINED};

/// One sandbox per test: a `vault/` data dir (database plus the object
/// store under `vault/media/`) and scratch space for source files.
struct Env {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn storage(&self) -> Storage {
        fs::create_dir_all(self.data_dir()).unwrap();
        Storage::open(self.data_dir().join("vault.db")).unwrap()
    }

    fn store(&self) -> ObjectStore {
        ObjectStore::new(&self.data_dir())
    }
}

fn no_cancel() -> bool {
    false
}

fn no_progress(_: u64, _: u64, _: &str) {}

/// The canonical row id of an archived message, by external id (the
/// same raw-read the recovery-proof suite uses).
fn message_id(s: &Storage, external_id: &str) -> i64 {
    s.connection()
        .query_row(
            "SELECT id FROM archive_messages WHERE external_id = ?1",
            [external_id],
            |r| r.get(0),
        )
        .unwrap()
}

fn original_rows(s: &Storage, table: &str) -> i64 {
    s.connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// The fidelity label of one record: retained originals name their
/// digest; everything else carries the shared constant (A8 consumes
/// the same constant later).
fn fidelity_label(s: &Storage, message_row: i64) -> String {
    match s.original_for_message(message_row).unwrap() {
        Some(original) => format!("original retained ({})", original.digest),
        None => ORIGINAL_NOT_RETAINED.to_string(),
    }
}

/// Open one message's retained original bytes through the object
/// store, exactly the way an "open original" surface would.
fn open_original(env: &Env, s: &Storage, message_row: i64) -> Vec<u8> {
    let original = s
        .original_for_message(message_row)
        .unwrap()
        .expect("record carries a retained original");
    let store = env.store();
    let name = store
        .object_name(&original.digest)
        .expect("the retained digest resolves to a store object");
    let path = store.resolve_object(&format!("media/{name}")).unwrap();
    fs::read(path).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// A picked `.eml` folder with two mails whose exact bytes are known.
fn eml_pick(env: &Env) -> (PathBuf, Vec<&'static [u8]>) {
    const EML_A: &[u8] = b"From: ana@example.com\nTo: bane@example.com\nSubject: Retention A\nMessage-ID: <ret-a@example.com>\nDate: Fri, 13 Nov 2026 10:00:00 +0100\n\nOriginal bytes A.\n";
    const EML_B: &[u8] = b"From: ana@example.com\nTo: bane@example.com\nSubject: Retention B\nMessage-ID: <ret-b@example.com>\nDate: Fri, 13 Nov 2026 11:00:00 +0100\n\nOriginal bytes B.\n";
    let dir = env.root.join("emls");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.eml"), EML_A).unwrap();
    fs::write(dir.join("b.eml"), EML_B).unwrap();
    (dir, vec![EML_A, EML_B])
}

#[test]
fn retention_off_by_default_writes_no_rows_and_labels_records() {
    let env = Env::new("off-by-default");
    let (pick, _bytes) = eml_pick(&env);
    let mut storage = env.storage();

    // The existing entry point — the one the app's command and job call
    // — keeps its signature and retains nothing.
    let outcome =
        mbox::import_pick_bounded(&mut storage, &pick, &env.data_dir(), &no_cancel, &mut no_progress)
            .unwrap();
    assert!(!outcome.cancelled);
    assert_eq!(outcome.summary.inserted, 2);
    assert!(
        outcome.originals.is_none(),
        "no retention pass ran for a default import"
    );

    // No original rows at all, and every record is honestly labeled.
    assert_eq!(original_rows(&storage, "original_objects"), 0);
    assert_eq!(original_rows(&storage, "original_references"), 0);
    for external_id in ["ret-a@example.com", "ret-b@example.com"] {
        let id = message_id(&storage, external_id);
        assert!(storage.original_for_message(id).unwrap().is_none());
        assert_eq!(fidelity_label(&storage, id), "original not retained");
    }
    assert_eq!(ORIGINAL_NOT_RETAINED, "original not retained");
}

#[test]
fn retained_import_stores_exact_raw_bytes_that_survive_source_deletion() {
    let env = Env::new("retained");
    let (pick, raw) = eml_pick(&env);
    let mut storage = env.storage();

    let outcome = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 2);
    let originals = outcome.originals.expect("the retention pass ran");
    assert_eq!(originals.stored, 2);
    assert_eq!(originals.deduped, 0);
    assert_eq!(
        originals.new_bytes,
        raw.iter().map(|b| b.len() as u64).sum::<u64>(),
        "the storage estimate is the sum of the new objects' bytes"
    );
    assert_eq!(original_rows(&storage, "original_objects"), 2);
    assert_eq!(original_rows(&storage, "original_references"), 2);

    // The source files are gone: the originals still open through the
    // object store, byte-exact, under the digest of their contents.
    fs::remove_dir_all(&pick).unwrap();
    assert!(!pick.exists());
    for (external_id, bytes) in [
        ("ret-a@example.com", raw[0]),
        ("ret-b@example.com", raw[1]),
    ] {
        let id = message_id(&storage, external_id);
        let original = storage.original_for_message(id).unwrap().unwrap();
        assert_eq!(original.digest, sha256_hex(bytes));
        assert_eq!(original.size, bytes.len() as u64);
        assert_eq!(open_original(&env, &storage, id), bytes);
        assert!(fidelity_label(&storage, id).starts_with("original retained"));
    }

    // The store holds exactly the two originals — no staging litter.
    let report = env
        .store()
        .reconcile(&storage.media_digests().unwrap(), false)
        .unwrap();
    assert!(report.is_clean(), "store reconciles clean: {report:?}");
}

#[test]
fn reimport_with_retention_adds_zero_new_original_objects() {
    let env = Env::new("dedup");
    let (pick, raw) = eml_pick(&env);
    let mut storage = env.storage();

    let first = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(first.originals.unwrap().stored, 2);

    // Same bytes again: digest dedup means nothing new is stored.
    let second = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(second.summary.skipped, 2, "messages dedup as before");
    let originals = second.originals.expect("the retention pass ran");
    assert_eq!(originals.stored, 0, "zero new original objects");
    assert_eq!(originals.deduped, 2);
    assert_eq!(originals.new_bytes, 0);
    assert_eq!(original_rows(&storage, "original_objects"), 2);
    assert_eq!(original_rows(&storage, "original_references"), 2);

    // And the object store still holds exactly the two objects.
    let objects = env.store().enumerate();
    assert_eq!(objects.len(), 2);
    let total: u64 = raw.iter().map(|b| b.len() as u64).sum();
    assert_eq!(objects.iter().map(|o| o.size).sum::<u64>(), total);
}

#[test]
fn retention_on_a_reimport_links_originals_to_existing_rows() {
    // A record first archived without retention, then re-imported with
    // the flag: the message row stays the same one (dedup), and the
    // original links to it — retention describes stored bytes, never
    // which run first saw the message.
    let env = Env::new("late-retention");
    let (pick, raw) = eml_pick(&env);
    let mut storage = env.storage();

    mbox::import_pick_bounded(&mut storage, &pick, &env.data_dir(), &no_cancel, &mut no_progress)
        .unwrap();
    assert_eq!(original_rows(&storage, "original_objects"), 0);
    let id_before = message_id(&storage, "ret-a@example.com");
    assert_eq!(fidelity_label(&storage, id_before), "original not retained");

    let second = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(second.summary.skipped, 2);
    let originals = second.originals.expect("the retention pass ran");
    assert_eq!(originals.stored, 2);
    assert_eq!(originals.new_bytes, raw.iter().map(|b| b.len() as u64).sum::<u64>());

    let id_after = message_id(&storage, "ret-a@example.com");
    assert_eq!(id_before, id_after, "the same archived row");
    assert_eq!(open_original(&env, &storage, id_after), raw[0]);
}

#[test]
fn mbox_retention_stores_each_part_without_its_separator_line() {
    // The `From sender date` line is an mbox container artifact, not
    // part of the RFC 5322 message: the retained original is the
    // message bytes, separator stripped — the same split the parser
    // normalizes from.
    let env = Env::new("mbox");
    let part_a: &[u8] =
        b"From: A <a@example.com>\nSubject: one\nMessage-ID: <mbox-ret-1@x>\n\nBody one.\n";
    let part_b: &[u8] =
        b"From: B <b@example.com>\nSubject: two\nMessage-ID: <mbox-ret-2@x>\n\nBody two.\n";
    let pick = env.root.join("archive.mbox");
    fs::write(
        &pick,
        format!(
            "From a@example.com Sun Nov  1 10:00:00 2026\n{}From b@example.com Sun Nov  1 11:00:00 2026\n{}",
            String::from_utf8_lossy(part_a),
            String::from_utf8_lossy(part_b),
        ),
    )
    .unwrap();

    let mut storage = env.storage();
    let outcome = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 2);
    let originals = outcome.originals.unwrap();
    assert_eq!(originals.stored, 2);
    assert_eq!(originals.new_bytes, (part_a.len() + part_b.len()) as u64);

    fs::remove_file(&pick).unwrap();
    for (external_id, bytes) in [("mbox-ret-1@x", part_a), ("mbox-ret-2@x", part_b)] {
        let id = message_id(&storage, external_id);
        assert_eq!(open_original(&env, &storage, id), bytes);
    }
}

/// The pick resolution keeps retention across every mail pick shape —
/// the wrapped-folder `.zip` expands and retains just like a plain
/// folder (the temp-tree guard keeps the bytes readable for the run).
#[test]
fn zipped_eml_pick_retains_originals_too() {
    use std::io::Write as _;
    let env = Env::new("zip");
    const EML: &[u8] = b"Subject: zipped\nMessage-ID: <ret-z@x>\n\nZipped body.\n";
    let zip_path = env.root.join("mail.zip");
    let file = fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("mail/1.eml", zip::write::FileOptions::<()>::default())
        .unwrap();
    zip.write_all(EML).unwrap();
    zip.finish().unwrap();

    let mut storage = env.storage();
    let outcome = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &zip_path,
        &env.data_dir(),
        true,
        &no_cancel,
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1);
    assert_eq!(outcome.originals.unwrap().stored, 1);

    fs::remove_file(&zip_path).unwrap();
    let id = message_id(&storage, "ret-z@x");
    assert_eq!(open_original(&env, &storage, id), EML);
}

/// A cancelled run claims no originals for its uncommitted tail — the
/// counters describe committed batches only (the same contract as the
/// ingest summary).
#[test]
fn cancelled_run_reports_no_uncommitted_originals() {
    let env = Env::new("cancel");
    let (pick, _raw) = eml_pick(&env);
    let mut storage = env.storage();

    let outcome = mbox::import_pick_bounded_with_retention(
        &mut storage,
        &pick,
        &env.data_dir(),
        true,
        &|| true, // cancelled before the first item
        &mut no_progress,
    )
    .unwrap();
    assert!(outcome.cancelled);
    assert_eq!(outcome.summary.inserted, 0);
    assert!(
        outcome.originals.map(|o| o.stored).unwrap_or(0) == 0,
        "a cancelled run stores no originals"
    );
    assert_eq!(original_rows(&storage, "original_objects"), 0);
    assert_eq!(original_rows(&storage, "original_references"), 0);
    let _ = Path::new(&pick);
}
