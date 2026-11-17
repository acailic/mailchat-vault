//! Merkle inclusion-proof tamper semantics over a written HTML export
//! bundle (P3·20): flipping one message's digest in the manifest must
//! fail the full-root check and that message's single-message check,
//! while a sibling's original proof still proves the original root and
//! fails against the tampered root — and a root mismatch marks the
//! whole bundle invalid, exactly and only when the root does not
//! recompute.

use std::path::PathBuf;

use vault_core::export_html::{
    export_html, verify_bundle_full, verify_bundle_message, HtmlExportOutcome, HtmlExportScope,
};
use vault_core::object_store::ObjectStore;
use vault_core::search2::SearchArchiveRequest;
use vault_core::storage::Storage;

use super::util::{msg, scratch_dir};

fn seeded() -> Storage {
    let storage = Storage::open_in_memory().unwrap();
    for (id, body) in [
        ("m1", "alpha"),
        ("m2", "beta"),
        ("m3", "gamma"),
        ("m4", "delta"),
    ] {
        storage.insert_message(&msg("merkle", id, body)).unwrap();
    }
    storage
}

fn write_bundle(storage: &Storage, dir: &std::path::Path) -> PathBuf {
    let objects = ObjectStore::new(dir);
    let dest = dir.join("bundle");
    match export_html(
        storage,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default(),
        },
        &dest,
        &objects,
        &|| false,
    )
    .unwrap()
    .unwrap()
    {
        HtmlExportOutcome::Complete(_) => dest,
        HtmlExportOutcome::Cancelled => panic!("uncancelled bundle must complete"),
    }
}

#[test]
fn flipping_one_digest_fails_full_check_and_that_messages_single_check() {
    let storage = seeded();
    let dir = scratch_dir();
    let bundle = write_bundle(&storage, dir.path());

    let pristine = verify_bundle_full(&bundle).unwrap();
    assert!(pristine.valid, "the untampered bundle verifies");
    let original_root = pristine.stated_root.clone().unwrap();
    let manifest_path = bundle.join("manifest");
    let text = std::fs::read_to_string(&manifest_path).unwrap();

    // Tamper: flip one hex digit of m1's recorded digest, leaving the
    // root line untouched (the after-the-fact edit the proofs exist to
    // catch).
    let m1_line = text
        .lines()
        .find(|l| l.starts_with("proof:") && l.ends_with(":m1"))
        .unwrap()
        .to_string();
    let digest: String = m1_line.split(':').nth(1).unwrap().to_string();
    let flipped_char = if digest.starts_with('0') { '1' } else { '0' };
    let flipped_digest = format!("{flipped_char}{}", &digest[1..]);
    let tampered_line = m1_line.replacen(&digest, &flipped_digest, 1);
    let tampered = text.replacen(&m1_line, &tampered_line, 1);
    std::fs::write(&manifest_path, &tampered).unwrap();

    // Full-root check: the recomputed root no longer matches the
    // recorded one — a root mismatch marks the whole bundle invalid.
    let full = verify_bundle_full(&bundle).unwrap();
    assert!(!full.valid, "root mismatch = bundle invalid");
    assert_eq!(full.stated_root.as_deref(), Some(original_root.as_str()));
    let tampered_root = full.recomputed_root.clone().unwrap();
    assert_ne!(tampered_root, original_root);

    // The flipped message's own single check fails: its unchanged path
    // no longer folds to the recorded root.
    let flipped = verify_bundle_message(&bundle, "m1")
        .unwrap()
        .expect("m1 still listed");
    assert!(!flipped.path_valid);
    assert_ne!(flipped.proved_root, original_root);

    // The sibling's original proof still validates against the
    // ORIGINAL root — and fails against the tampered root (the one the
    // tampered digests recompute to). Its own path is intact, yet the
    // bundle stays invalid: a single passing path never rescues a root
    // mismatch.
    let sibling = verify_bundle_message(&bundle, "m2")
        .unwrap()
        .expect("m2 still listed");
    assert_eq!(
        sibling.proved_root, original_root,
        "still validates against the original root"
    );
    assert_ne!(
        sibling.proved_root, tampered_root,
        "fails against the tampered root"
    );
    assert!(!full.valid);

    // The other side of the semantics: rewrite the root line to the
    // tampered data's recomputed root. Now the root recomputes, so the
    // full check passes — but every ORIGINAL proof names the original
    // root and fails against the rewritten one. A proof validates
    // against the tampered root only in that self-consistent sense;
    // comparing the recorded root against the one saved at export time
    // is what reveals the swap.
    let reforged = tampered.replacen(
        &format!("merkle-root: {original_root}"),
        &format!("merkle-root: {tampered_root}"),
        1,
    );
    std::fs::write(&manifest_path, &reforged).unwrap();
    let reforged_full = verify_bundle_full(&bundle).unwrap();
    assert!(
        reforged_full.valid,
        "a rewritten root recomputes over the tampered digests"
    );
    let sibling_reforged = verify_bundle_message(&bundle, "m2")
        .unwrap()
        .expect("m2 still listed");
    assert!(!sibling_reforged.path_valid);
    assert_eq!(
        sibling_reforged.proved_root, original_root,
        "the original proof still names the original root"
    );
}
