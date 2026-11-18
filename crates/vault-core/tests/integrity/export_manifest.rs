//! Tamper-evident verification manifest for HTML export bundles (A9):
//! every finished bundle carries `manifest.json` chaining per-message
//! digests, covering receipt rows, and page/asset SHA-256s into one
//! head hash; `verify_bundle` recomputes the chain from the bundle
//! alone and names the first divergence. These tests pin the two
//! honesty claims the feature stands on: re-exports of unchanged data
//! reproduce the head hash, and any later change to a chained file or
//! recorded digest is reported by name.
//!
//! Acceptance mapping (task A9):
//! - `head_hash_is_stable_across_reexports_and_verify_passes`
//! - `tampering_with_a_page_file_fails_naming_that_pages_first_message`
//! - `tampering_with_manifest_json_fails_naming_the_divergence`
//! - `a_bundle_without_manifest_json_reports_clearly_incomplete`
//! - `tampering_with_an_asset_names_the_asset`
//! - `tampering_with_a_receipt_row_breaks_the_head_hash`

use std::fs;
use std::path::{Path, PathBuf};

use vault_core::export_html::{export_html, HtmlExportOutcome, HtmlExportScope, PAGE_MESSAGES};
use vault_core::export_manifest::{
    verify_bundle, VerificationManifest, BUNDLE_README, MANIFEST_JSON,
};
use vault_core::message::NewMessage;
use vault_core::object_store::ObjectStore;
use vault_core::search2::SearchArchiveRequest;
use vault_core::storage::Storage;

/// One sandbox per test: a data dir (database plus object store) and
/// room for bundles and tampered copies.
struct Env {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        fs::create_dir_all(root.join("data")).unwrap();
        Self { keeper, root }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn storage(&self) -> Storage {
        Storage::open(self.data_dir().join("vault.db")).unwrap()
    }

    fn bundle(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

/// One `.eml` whose exact raw bytes are known, imported with original
/// retention so the manifest's `original_digest` seam has a real row.
const EML: &[u8] = b"From: ana@example.com\nTo: bane@example.com\nSubject: Chain A\nMessage-ID: <chain-a@example.com>\nDate: Fri, 13 Nov 2026 10:00:00 +0100\n\nRetained original bytes A.\n";

fn import_retained_eml(env: &Env, storage: &mut Storage) {
    let pick = env.root.join("emls");
    fs::create_dir_all(&pick).unwrap();
    fs::write(pick.join("a.eml"), EML).unwrap();
    let outcome = vault_core::mbox::import_pick_bounded_with_retention(
        storage,
        &pick,
        &env.data_dir(),
        true,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1);
    assert!(outcome.originals.expect("the retention pass ran").stored > 0);
}

/// An email-shaped row for the batch path (which records the import
/// receipt).
fn email(id: &str, day: u8, body: &str) -> NewMessage {
    NewMessage::new(
        "mbox",
        id,
        "ana <ana@example.com>",
        &format!("2026-11-{day:02}T09:00:00Z"),
        body,
    )
}

/// Store one media object and return the `raw_json` media entry the
/// archive links it through (the shape `vault_core::media` writes).
fn stored_media_entry(env: &Env, name: &str, bytes: &[u8]) -> serde_json::Value {
    let src = env.root.join("incoming");
    fs::write(&src, bytes).unwrap();
    let (entry, _written) = vault_core::media::store(
        &env.data_dir(),
        &src,
        name,
        "image",
        &mut vault_core::object_store::Capacity::default(),
    )
    .unwrap();
    serde_json::json!({
        "sha256": entry.sha256,
        "path": entry.path,
        "name": entry.name,
        "kind": entry.kind,
        "size": entry.size,
    })
}

/// The shared fixture: one retained-original mail import (with its
/// receipt), one batch import of `extra` rows (with its receipt, one
/// row carrying a stored asset), and one direct insert whose instance
/// has no receipts. Returns the stored messages' external ids in no
/// particular order.
fn seed(env: &Env, extra: Vec<NewMessage>) -> Storage {
    let mut storage = env.storage();
    import_retained_eml(env, &mut storage);
    let mut rows = Vec::new();
    for (index, mut row) in extra.into_iter().enumerate() {
        if index == 0 {
            let media = stored_media_entry(env, "photo.jpg", b"jpeg bytes here");
            let mut obj = serde_json::Map::new();
            obj.insert("media".into(), serde_json::json!([media]));
            row.raw_json = serde_json::Value::Object(obj).to_string();
        }
        rows.push(row);
    }
    storage
        .commit_import_batch("batch-1", "mbox", &rows, &[], &[], 0, true)
        .unwrap();
    storage
}

/// Run an export of everything into `dest` and return the summary.
fn export_all(
    env: &Env,
    storage: &Storage,
    dest: &Path,
) -> vault_core::export_html::HtmlExportSummary {
    let objects = ObjectStore::new(&env.data_dir());
    match export_html(
        storage,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default(),
        },
        dest,
        &objects,
        &|| false,
    )
    .unwrap()
    {
        Some(HtmlExportOutcome::Complete(summary)) => summary,
        other => panic!("expected a completed export, got {other:?}"),
    }
}

/// Read and parse a bundle's verification manifest.
fn manifest_of(bundle: &Path) -> VerificationManifest {
    serde_json::from_str(&fs::read_to_string(bundle.join(MANIFEST_JSON)).unwrap()).unwrap()
}

/// Recursively copy a bundle (small: test fixtures only).
fn copy_bundle(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_bundle(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

// ---------------------------------------------------------------------------

/// The stability claim: re-exporting unchanged data reproduces the
/// same head hash byte-for-byte, verify passes on a pristine bundle,
/// receipts ride along, and retained originals carry their digest.
#[test]
fn head_hash_is_stable_across_reexports_and_verify_passes() {
    let env = Env::new("stable");
    let storage = seed(
        &env,
        vec![
            email("batch-1@e.com", 2, "first batch body"),
            email("batch-2@e.com", 3, "second batch body"),
        ],
    );

    let first = export_all(&env, &storage, &env.bundle("one"));
    let second = export_all(&env, &storage, &env.bundle("two"));

    // The summary repeats the manifest's head hash, and both exports
    // agree on it — the bundle bytes are byte-stable.
    assert_eq!(first.head_hash, second.head_hash);
    assert!(!first.head_hash.is_empty());
    assert_eq!(
        fs::read(env.bundle("one").join(MANIFEST_JSON)).unwrap(),
        fs::read(env.bundle("two").join(MANIFEST_JSON)).unwrap(),
        "re-exports of the same data are byte-identical"
    );
    let manifest = manifest_of(&env.bundle("one"));
    assert_eq!(manifest.head_hash, first.head_hash);
    assert_eq!(manifest.messages.len() as u64, first.written);
    assert_eq!(manifest.version, 1);

    // Every message entry carries the stored digest and its page.
    assert!(manifest
        .messages
        .iter()
        .all(|m| m.message_digest.len() == 64 && m.page.starts_with("page-")));
    let external_ids: Vec<&str> = manifest
        .messages
        .iter()
        .map(|m| m.external_id.as_str())
        .collect();
    assert!(external_ids.contains(&"chain-a@example.com"));
    assert!(external_ids.contains(&"batch-1@e.com"));

    // The retained original names its digest; the others carry none.
    let retained = manifest
        .messages
        .iter()
        .find(|m| m.external_id == "chain-a@example.com")
        .unwrap();
    assert_eq!(
        retained.original_digest.as_deref(),
        Some(sha256_hex(EML).as_str())
    );
    assert!(manifest
        .messages
        .iter()
        .filter(|m| m.external_id != "chain-a@example.com")
        .all(|m| m.original_digest.is_none()));

    // The import receipts covering the mbox instance ride along,
    // serialized with the stored shape.
    assert!(manifest
        .receipts
        .iter()
        .any(|r| r.kind.as_str() == "import"));

    // The README ships beside the manifest and states both directions.
    let readme = fs::read_to_string(env.bundle("one").join(BUNDLE_README)).unwrap();
    assert!(readme.contains("tamper **evidence**"));
    assert!(readme.contains("third-party notarization"));

    // Verify passes on the pristine bundles and reports the head.
    for bundle in ["one", "two"] {
        let report = verify_bundle(&env.bundle(bundle)).unwrap();
        assert!(report.ok, "{bundle}: {report:?}");
        assert_eq!(report.head_hash, first.head_hash);
        assert_eq!(report.messages, first.written);
        assert_eq!(report.files, first.pages + first.assets_copied);
        assert!(report.first_divergence.is_none());
    }
}

/// The tamper evidence: one changed byte in one page file of a copied
/// bundle makes verify fail naming that page's first message, while
/// the untouched original still passes. A deleted page is reported
/// the same way.
#[test]
fn tampering_with_a_page_file_fails_naming_that_pages_first_message() {
    let rows: Vec<NewMessage> = (0..(PAGE_MESSAGES as u8 + 5))
        .map(|i| {
            email(
                &format!("m{i}@e.com"),
                (i % 27) + 1,
                &format!("body number {i}"),
            )
        })
        .collect();
    let env = Env::new("page-tamper");
    let storage = seed(&env, rows);
    let original = env.bundle("original");
    let summary = export_all(&env, &storage, &original);
    assert!(summary.pages >= 2, "the fixture must span pages");

    let manifest = manifest_of(&original);
    let second_page = "page-0002.html";
    let first_on_page = manifest
        .messages
        .iter()
        .find(|m| m.page == second_page)
        .expect("the second page has messages");
    assert!(manifest
        .messages
        .iter()
        .any(|m| m.external_id == first_on_page.external_id && m.page == second_page));

    // A tampered copy: flip one byte inside the page body.
    let tampered = env.bundle("tampered");
    copy_bundle(&original, &tampered);
    let page_path = tampered.join(second_page);
    let mut bytes = fs::read(&page_path).unwrap();
    let last = bytes.len() - 3;
    bytes[last] = b'X';
    fs::write(&page_path, bytes).unwrap();

    let report = verify_bundle(&tampered).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(
        divergence.contains(second_page),
        "names the page: {divergence}"
    );
    assert!(
        divergence.contains(&first_on_page.external_id),
        "names the page's first message: {divergence}"
    );
    // The untouched original still verifies.
    assert!(verify_bundle(&original).unwrap().ok);

    // A missing page is reported the same way — named, with the page's
    // first message.
    let deleted = env.bundle("deleted-page");
    copy_bundle(&original, &deleted);
    fs::remove_file(deleted.join(second_page)).unwrap();
    let report = verify_bundle(&deleted).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(divergence.contains(second_page));
    assert!(divergence.contains(&first_on_page.external_id));
}

/// Editing manifest.json itself breaks verification: a changed message
/// digest is reported at that message, a changed head hash at the
/// head.
#[test]
fn tampering_with_manifest_json_fails_naming_the_divergence() {
    let env = Env::new("manifest-tamper");
    let storage = seed(
        &env,
        vec![
            email("batch-1@e.com", 2, "first batch body"),
            email("batch-2@e.com", 3, "second batch body"),
        ],
    );
    let original = env.bundle("original");
    export_all(&env, &storage, &original);
    let manifest = manifest_of(&original);
    assert!(manifest.messages.len() >= 3);

    // A changed message digest: the chain diverges at that message.
    let digest_tampered = env.bundle("digest-tampered");
    copy_bundle(&original, &digest_tampered);
    let mut edited = manifest_of(&original);
    let victim = &mut edited.messages[1];
    let victim_id = victim.external_id.clone();
    victim.message_digest = "0".repeat(64);
    victim.chain = manifest.messages[2].chain.clone(); // a plausible-looking lie
    fs::write(
        digest_tampered.join(MANIFEST_JSON),
        serde_json::to_string_pretty(&edited).unwrap(),
    )
    .unwrap();
    let report = verify_bundle(&digest_tampered).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(
        divergence.contains(&victim_id),
        "names the divergent message: {divergence}"
    );

    // A forged head hash: everything below recomputes, the head does
    // not match.
    let head_tampered = env.bundle("head-tampered");
    copy_bundle(&original, &head_tampered);
    let mut edited = manifest_of(&original);
    edited.head_hash = "f".repeat(64);
    fs::write(
        head_tampered.join(MANIFEST_JSON),
        serde_json::to_string_pretty(&edited).unwrap(),
    )
    .unwrap();
    let report = verify_bundle(&head_tampered).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(
        divergence.contains("head hash"),
        "names the head: {divergence}"
    );

    // A corrupted (unparseable) manifest is a divergence, not a pass.
    let garbage = env.bundle("garbage");
    copy_bundle(&original, &garbage);
    fs::write(garbage.join(MANIFEST_JSON), b"{ not json").unwrap();
    let report = verify_bundle(&garbage).unwrap();
    assert!(!report.ok);
    assert!(report
        .first_divergence
        .unwrap()
        .contains("cannot be parsed"));
}

/// A bundle without manifest.json is clearly incomplete — the answer
/// is a report, never a silent pass.
#[test]
fn a_bundle_without_manifest_json_reports_clearly_incomplete() {
    let env = Env::new("no-manifest");
    let storage = seed(&env, vec![email("batch-1@e.com", 2, "a body")]);
    let original = env.bundle("original");
    export_all(&env, &storage, &original);

    let stripped = env.bundle("stripped");
    copy_bundle(&original, &stripped);
    fs::remove_file(stripped.join(MANIFEST_JSON)).unwrap();
    let report = verify_bundle(&stripped).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(divergence.contains("manifest.json is missing"));
    assert!(divergence.to_lowercase().contains("incomplete"));

    // A directory that never was a bundle answers the same way.
    let empty = env.bundle("empty");
    fs::create_dir_all(&empty).unwrap();
    let report = verify_bundle(&empty).unwrap();
    assert!(!report.ok);
    assert!(report
        .first_divergence
        .unwrap()
        .contains("manifest.json is missing"));
}

/// A changed asset byte is reported by the asset's bundle path.
#[test]
fn tampering_with_an_asset_names_the_asset() {
    let env = Env::new("asset-tamper");
    let storage = seed(&env, vec![email("batch-1@e.com", 2, "body with a photo")]);
    let original = env.bundle("original");
    let summary = export_all(&env, &storage, &original);
    assert_eq!(summary.assets_copied, 1);
    let asset = manifest_of(&original)
        .files
        .iter()
        .find(|f| f.path.starts_with("assets/"))
        .unwrap()
        .path
        .clone();

    let tampered = env.bundle("tampered");
    copy_bundle(&original, &tampered);
    let path = tampered.join(&asset);
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 0xFF;
    fs::write(&path, bytes).unwrap();
    let report = verify_bundle(&tampered).unwrap();
    assert!(!report.ok);
    let divergence = report.first_divergence.unwrap();
    assert!(divergence.contains(&asset), "names the asset: {divergence}");
    assert!(verify_bundle(&original).unwrap().ok);
}

/// Receipt rows are chained: editing one in manifest.json breaks the
/// recomputed head hash even though every file still matches.
#[test]
fn tampering_with_a_receipt_row_breaks_the_head_hash() {
    let env = Env::new("receipt-tamper");
    let storage = seed(&env, vec![email("batch-1@e.com", 2, "a body")]);
    let original = env.bundle("original");
    export_all(&env, &storage, &original);
    let manifest = manifest_of(&original);
    assert!(
        !manifest.receipts.is_empty(),
        "the fixture must carry receipt rows"
    );

    let tampered = env.bundle("tampered");
    copy_bundle(&original, &tampered);
    let mut edited = manifest.clone();
    edited.receipts[0].new_records += 1;
    fs::write(
        tampered.join(MANIFEST_JSON),
        serde_json::to_string_pretty(&edited).unwrap(),
    )
    .unwrap();
    let report = verify_bundle(&tampered).unwrap();
    assert!(!report.ok);
    assert!(report
        .first_divergence
        .unwrap()
        .contains("head hash does not match"));
}

/// SHA-256 as lowercase hex, the release-checksum convention.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
