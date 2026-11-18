// A6: the attachment projection (files.rs). One indexed projection
// spans the linked references inside archived messages and the
// unlinked objects in the media registry; a resumable backfill fills
// it without mutating raw records, and the files read models answer
// from it. These tests prove the three acceptance bullets plus the
// no-op re-run:
//
// 1. a backfill stopped by batch limit commits its partial work and
//    resumes from the checkpoint through the existing job runner, with
//    the coverage check passing on completion (and a completed re-run
//    scanning nothing);
// 2. identical bytes attached in two messages group as one unique file
//    whose references query lists every referring message;
// 3. unlinked whole-tree objects appear in the files view without an
//    invented message association;
// 4. missing references stay inspectable through the explicit
//    availability filter.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vault_core::chat_export::import_bounded;
use vault_core::files::{
    self, FilesBackfillOutcome, FilesSearchPage, FilesSearchRequest, ThumbnailStatus,
};
use vault_core::jobs::{DbPool, JobId, JobState, PoolConfig};
use vault_core::storage::Storage;

/// Real small files committed with the repo (the same fixtures the
/// media suites use).
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

/// One temp workspace per test: an `export/` tree and a `data/`
/// app-data dir, so store writes never touch the repo (the
/// media_findings sandbox pattern).
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

    /// Copy a fixture into the export tree at `rel`.
    fn put(&self, rel: &str, fixture_name: &str) {
        let target = self.export_dir().join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(fixture(fixture_name), &target).unwrap();
    }
}

const STRAY_NAME: &str = "stray-map.png";
const STRAY_BYTES: &[u8] = b"stray export bytes that no message references";
const MISSING_NAME: &str = "does-not-exist.jpg";

/// A Telegram export tree covering the projection's whole domain:
///
/// - two messages attaching the **same bytes** (photo.jpg under two
///   export paths — content addressing stores one object, two
///   references);
/// - one message attaching plan.pdf (a second unique file);
/// - one message whose referenced file is **absent** from the export
///   (recorded as a missing entry with its reason);
/// - one text-only message (no references at all);
/// - one unreferenced stray file (stored and registered by the
///   whole-tree import's unlinked pass, belonging to no message).
///
/// Returns the digest of the shared photo.
fn fixture_vault(sb: &Sandbox, storage: &mut Storage) -> String {
    sb.put("photos/photo_101@01-11-2023.jpg", "photo.jpg");
    sb.put("photos/photo_102@01-11-2023.jpg", "photo.jpg");
    sb.put("files/plan.pdf", "plan.pdf");
    fs::write(sb.export_dir().join(STRAY_NAME), STRAY_BYTES).unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{
  "name": "Arhiva ekipa", "type": "private_group", "id": 123456789,
  "messages": [
    { "id": 101, "type": "message", "date_unixtime": "1698832920",
      "from": "Ana", "photo": "photos/photo_101@01-11-2023.jpg", "text": "" },
    { "id": 102, "type": "message", "date_unixtime": "1698832980",
      "from": "Marko", "photo": "photos/photo_102@01-11-2023.jpg", "text": "" },
    { "id": 103, "type": "message", "date_unixtime": "1698833040",
      "from": "Jelena", "file": "files/plan.pdf", "text": "" },
    { "id": 104, "type": "message", "date_unixtime": "1698833100",
      "from": "Petar", "photo": "photos/does-not-exist.jpg", "text": "" },
    { "id": 105, "type": "message", "date_unixtime": "1698833160",
      "from": "Milica", "text": "plain text, nothing attached" }
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
    assert_eq!(outcome.summary.inserted, 5, "{:?}", outcome.summary);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.stored, 2, "{media:?}"); // photo + plan (stray is unlinked, below)
    assert_eq!(media.deduped, 1, "{media:?}"); // the second photo copy
    assert_eq!(media.missing, 1, "{media:?}"); // the ghost photo
    assert_eq!(media.unlinked, 1, "{media:?}"); // the stray file
    sha256_hex(&fixture("photo.jpg"))
}

fn wait_terminal(pool: &Arc<DbPool>, id: JobId) -> vault_core::jobs::JobRecord {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(record) = pool.job(id) {
            if record.state.is_terminal() {
                return record;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("job never became terminal: {:?}", pool.job(id));
}

/// Submit the backfill through the existing job runner (the only
/// submission path; no new scheduler) and return its terminal record.
fn backfill_via_job(pool: &Arc<DbPool>, batch_limit: Option<u64>) -> vault_core::jobs::JobRecord {
    let id = pool
        .submit_job(files::FILES_BACKFILL_KIND, move |ctx| {
            files::run_backfill_job(ctx, batch_limit, false)
        })
        .unwrap();
    wait_terminal(pool, id)
}

fn outcome_of(record: &vault_core::jobs::JobRecord) -> FilesBackfillOutcome {
    match &record.state {
        JobState::Succeeded { result } => {
            serde_json::from_value::<FilesBackfillOutcome>(result.clone()).unwrap()
        }
        other => panic!("backfill job did not succeed: {other:?}"),
    }
}

fn search(storage: &Storage, request: &FilesSearchRequest) -> FilesSearchPage {
    files::files_search(storage, request).unwrap()
}

/// The vault file with the fixture imported, then the pool opened over
/// it (the import needs a `&mut` storage handle; the pool takes over
/// the file afterwards).
fn pooled_fixture_vault(sb: &Sandbox, db: &Path) -> Arc<DbPool> {
    {
        let mut storage = Storage::open(db).unwrap();
        fixture_vault(sb, &mut storage);
    }
    DbPool::spawn(db, PoolConfig::default(), |_| {}).unwrap()
}

// ---------------------------------------------------------------------------
// 1. Resumable backfill: stop by batch limit, resume, coverage passes
// ---------------------------------------------------------------------------

#[test]
fn backfill_stops_at_a_batch_limit_and_resumes_to_coverage_success() {
    let sb = Sandbox::new("files-resume");
    let tmp = tempfile::tempdir().unwrap();
    let pool = pooled_fixture_vault(&sb, &tmp.path().join("vault.db"));

    // The killed run: a batch limit of 2 messages stops mid-vault (4
    // of the 5 messages carry references). Committed work stands.
    let first = outcome_of(&backfill_via_job(&pool, Some(2)));
    assert!(!first.completed, "{first:?}");
    assert_eq!(first.scanned_messages, 2, "{first:?}");
    assert!(first.refs_upserted >= 2, "{first:?}");
    assert!(
        first.coverage.is_none(),
        "a partial run never claims coverage"
    );
    let (partial_rows, checkpoint) = pool.with_read(|s| {
        let rows = s
            .connection()
            .query_row("SELECT COUNT(*) FROM attachment_refs", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        (rows, files::read_checkpoint(s.connection()).unwrap())
    });
    assert!(partial_rows >= 2, "the stopped run committed its batch");
    assert!(partial_rows < 4, "the projection is not yet complete");

    // The resume: no limit, runs to the end from the checkpoint, and
    // the coverage check passes.
    let second = outcome_of(&backfill_via_job(&pool, None));
    assert!(second.completed, "{second:?}");
    assert_eq!(second.resumed_from, checkpoint, "{second:?}");
    assert_eq!(second.scanned_messages, 2, "{second:?}"); // the two remaining media messages
    let coverage = second.coverage.expect("completed run verified coverage");
    assert_eq!(coverage.raw_references, coverage.projection_rows);
    assert_eq!(coverage.projection_rows, 4, "{coverage:?}"); // 2 photo refs + plan + ghost

    // A completed re-run is a no-op: the checkpoint skips everything.
    let third = outcome_of(&backfill_via_job(&pool, None));
    assert!(third.completed, "{third:?}");
    assert_eq!(third.scanned_messages, 0, "{third:?}");
    assert_eq!(third.refs_upserted, 0, "{third:?}");
    assert!(third
        .coverage
        .expect("re-run re-verifies coverage")
        .is_complete());

    pool.shutdown();
}

// ---------------------------------------------------------------------------
// 2. Same bytes in two messages: one unique file, every referrer listed
// ---------------------------------------------------------------------------

#[test]
fn identical_bytes_attached_twice_group_as_one_unique_file() {
    let sb = Sandbox::new("files-group");
    let mut storage = Storage::open_in_memory().unwrap();
    let photo_sha = fixture_vault(&sb, &mut storage);
    let outcome = files::run_backfill(&storage, None).unwrap();
    assert!(outcome.completed, "{outcome:?}");

    // One unique file for the shared bytes, occurrence count 2 — the
    // unique-file grouping distinct from the occurrence total. (The
    // stray registry object is an image too; the digest pin below
    // finds the photo group among however many match.)
    let page = search(
        &storage,
        &FilesSearchRequest {
            kind: Some("image".into()),
            availability: Some("preserved".into()),
            ..FilesSearchRequest::default()
        },
    );
    let group = page
        .files
        .iter()
        .find(|f| f.sha256.as_deref() == Some(photo_sha.as_str()))
        .expect("the shared photo is one unique file");
    assert_eq!(
        page.files
            .iter()
            .filter(|f| f.sha256.as_deref() == Some(photo_sha.as_str()))
            .count(),
        1,
        "{:?}",
        page.files
    );
    assert_eq!(group.occurrences, 2, "{group:?}");
    assert_eq!(group.availability, "preserved");
    assert!(!group.unlinked);

    // The references query lists EVERY referring message with its
    // provenance — both messages, oldest first.
    let references = files::file_references(&storage, &photo_sha, 0, 50).unwrap();
    assert_eq!(references.total, 2, "{references:?}");
    let ids: Vec<i64> = references.references.iter().map(|r| r.message_id).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids[0] < ids[1], "oldest message first: {ids:?}");
    for row in &references.references {
        assert!(!row.source_instance_id.is_empty(), "source provenance");
        assert!(!row.import_id.is_empty(), "import provenance: {row:?}");
        assert_eq!(row.availability, "preserved");
        assert!(
            row.name.contains("photo_10"),
            "kept the export name: {row:?}"
        );
    }
    let authors: Vec<&str> = references
        .references
        .iter()
        .map(|r| r.author.as_str())
        .collect();
    assert_eq!(authors, ["Ana", "Marko"]);

    // And the unique-file count is honest against the whole vault:
    // two preserved digests (photo, plan), one missing group (the
    // ghost), and the stray registry object — 4 unique files over 3
    // message references plus 1 unpreserved one.
    let all = search(&storage, &FilesSearchRequest::default());
    assert_eq!(all.files.len(), 4, "{:?}", all.files);
    let occurrences: u64 = all.files.iter().map(|f| f.occurrences).sum();
    assert_eq!(
        occurrences, 4,
        "2 photo + 1 plan + 1 ghost references: {all:?}"
    );

    // The thumbnail stub answers the availability fact for the digest.
    assert_eq!(
        files::thumbnail_get(&storage, &photo_sha).unwrap(),
        ThumbnailStatus { available: true }
    );
}

// ---------------------------------------------------------------------------
// 3. Unlinked registry objects: present, without invented messages
// ---------------------------------------------------------------------------

#[test]
fn unlinked_objects_appear_without_message_association() {
    let sb = Sandbox::new("files-unlinked");
    let mut storage = Storage::open_in_memory().unwrap();
    fixture_vault(&sb, &mut storage);
    files::run_backfill(&storage, None).unwrap();

    let stray_sha = {
        let row = &storage.unlinked_media_objects().unwrap()[0];
        assert_eq!(row.name, STRAY_NAME);
        row.sha256.clone()
    };

    // The files view carries it: registry provenance, no occurrences,
    // and no message association.
    let all = search(&storage, &FilesSearchRequest::default());
    let stray = all
        .files
        .iter()
        .find(|f| f.sha256.as_deref() == Some(stray_sha.as_str()))
        .expect("the unlinked object is in the files view");
    assert!(stray.unlinked, "{stray:?}");
    assert_eq!(stray.occurrences, 0, "{stray:?}");
    assert_eq!(stray.name, STRAY_NAME);
    assert_eq!(stray.availability, "preserved");

    // The references query resolves it to its registry provenance and
    // NO message — nothing invents an association.
    let references = files::file_references(&storage, &stray_sha, 0, 50).unwrap();
    assert_eq!(references.total, 0, "{references:?}");
    assert!(references.references.is_empty(), "{references:?}");
    let registry = references.registry.expect("registry provenance present");
    assert_eq!(registry.origin, "unlinked");
    assert_eq!(registry.size, STRAY_BYTES.len() as u64);

    // The thumbnail stub reports the availability fact for it too.
    assert_eq!(
        files::thumbnail_get(&storage, &stray_sha).unwrap(),
        ThumbnailStatus { available: true }
    );
}

// ---------------------------------------------------------------------------
// 4. Missing references stay inspectable (explicit availability filter)
// ---------------------------------------------------------------------------

#[test]
fn missing_references_surface_through_the_availability_filter() {
    let sb = Sandbox::new("files-missing");
    let mut storage = Storage::open_in_memory().unwrap();
    fixture_vault(&sb, &mut storage);
    files::run_backfill(&storage, None).unwrap();

    // Default view: every group — preserved digests, the unlinked
    // object, and the missing reference — each stamped with its own
    // availability.
    let all = search(&storage, &FilesSearchRequest::default());
    assert_eq!(all.files.len(), 4, "{:?}", all.files);

    // The explicit filter isolates the missing reference group: the
    // ghost photo, named, with its recorded reason.
    let missing = search(
        &storage,
        &FilesSearchRequest {
            availability: Some("missing".into()),
            ..FilesSearchRequest::default()
        },
    );
    assert_eq!(missing.files.len(), 1, "{:?}", missing.files);
    let group = &missing.files[0];
    assert_eq!(group.name, MISSING_NAME, "{group:?}");
    assert_eq!(group.sha256, None, "no digest exists for missing bytes");
    assert_eq!(group.occurrences, 1, "{group:?}");
    assert_eq!(
        group.reason, "not-in-export",
        "the recorded reason: {group:?}"
    );

    // And the inverse filter keeps it out.
    let preserved = search(
        &storage,
        &FilesSearchRequest {
            availability: Some("preserved".into()),
            ..FilesSearchRequest::default()
        },
    );
    assert_eq!(preserved.files.len(), 3, "{:?}", preserved.files); // photo + plan + stray
    assert!(preserved
        .files
        .iter()
        .all(|f| f.name != MISSING_NAME && f.availability == "preserved"));

    // An invalid filter value is a clear error, never a silent default.
    let err = files::files_search(
        &storage,
        &FilesSearchRequest {
            availability: Some("gone".into()),
            ..FilesSearchRequest::default()
        },
    );
    assert!(err.is_err());

    // Source and date filters constrain the message-anchored groups
    // (the photo/plan/ghost messages all date 2023-11-01 UTC): the
    // stray registry object carries no message date and stays out
    // under a date filter rather than borrowing one.
    let source = storage.list_source_instances().unwrap()[0].id.clone();
    let dated = search(
        &storage,
        &FilesSearchRequest {
            source_instance_id: Some(source),
            from_date: Some("2023-11-01".into()),
            to_date: Some("2023-11-01".into()),
            ..FilesSearchRequest::default()
        },
    );
    assert_eq!(dated.files.len(), 3, "{:?}", dated.files); // photo + plan + ghost
    assert!(
        dated.files.iter().all(|f| !f.unlinked),
        "registry objects carry no message date: {dated:?}"
    );
}

// ---------------------------------------------------------------------------
// Cursor pagination over the merged group space
// ---------------------------------------------------------------------------

#[test]
fn files_search_pages_stably_through_the_cursor() {
    let sb = Sandbox::new("files-cursor");
    let mut storage = Storage::open_in_memory().unwrap();
    fixture_vault(&sb, &mut storage);
    files::run_backfill(&storage, None).unwrap();

    // Page through everything two groups at a time: no duplicates, no
    // losses, stable order, and the last page carries no cursor.
    let mut seen: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = search(
            &storage,
            &FilesSearchRequest {
                cursor: cursor.clone(),
                limit: Some(2),
                ..FilesSearchRequest::default()
            },
        );
        assert!(!page.files.is_empty(), "empty page before exhaustion");
        seen.extend(page.files.iter().map(|f| f.sha256.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 4, "{seen:?}");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "no group appears twice");

    // A malformed cursor is rejected, never silently ignored.
    let err = files::files_search(
        &storage,
        &FilesSearchRequest {
            cursor: Some("not-a-cursor".into()),
            ..FilesSearchRequest::default()
        },
    );
    assert!(err.is_err());
}

// ---------------------------------------------------------------------------
// The projection never mutates raw records
// ---------------------------------------------------------------------------

#[test]
fn backfill_never_mutates_raw_records() {
    let sb = Sandbox::new("files-raw-immutable");
    let mut storage = Storage::open_in_memory().unwrap();
    fixture_vault(&sb, &mut storage);

    let before: Vec<(i64, String)> = {
        let conn = storage.connection();
        let mut stmt = conn
            .prepare("SELECT id, raw_json FROM archive_messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let revision_before = storage.archive_revision().unwrap();

    files::run_backfill(&storage, None).unwrap();

    let after: Vec<(i64, String)> = {
        let conn = storage.connection();
        let mut stmt = conn
            .prepare("SELECT id, raw_json FROM archive_messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(before, after, "raw records are untouched");
    assert_eq!(
        storage.archive_revision().unwrap(),
        revision_before,
        "the projection is not a content change"
    );
}
