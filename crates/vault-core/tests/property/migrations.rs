//! Migration contract: reopening an existing database applies migrations as
//! a strict no-op — schema version is stable, every stored row survives
//! byte-for-byte, dedup still sees prior rows, and a second open connection
//! (WAL) observes the same data.

use std::collections::HashMap;

use proptest::prelude::*;

use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::Storage;

use crate::fixtures;

proptest! {
    #![proptest_config(crate::prop_config(16))]

    // Reopen an existing file database 1..=3 times: nothing changes, no
    // migration re-runs (a re-run would fail on the existing schema), and
    // dedup keys from previous opens are still respected.
    #[test]
    fn reopening_is_a_migration_noop(
        batch in fixtures::batch(20),
        reopens in 1u8..=3,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vault.db");

        let first = Storage::open(&db).unwrap();
        let version = first.schema_version().unwrap();
        prop_assert!(version >= 1, "fresh database must be migrated");
        for msg in &batch {
            first.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);
        drop(first);

        for _ in 0..reopens {
            let reopened = Storage::open(&db).unwrap();
            prop_assert_eq!(reopened.schema_version().unwrap(), version);
            prop_assert_eq!(reopened.stats().unwrap().total, model.len() as u64);

            // Re-inserting the batch into the reopened database must be a
            // full no-op (dedup survives reopen), even with new content.
            for msg in &batch {
                let again = fixtures::mutate(msg);
                let outcome = reopened.insert_message(&again).unwrap();
                prop_assert!(
                    matches!(outcome, InsertOutcome::Duplicate(_)),
                    "dedup key lost across reopen: {}/{}",
                    again.source, again.external_id
                );
            }
            prop_assert_eq!(reopened.stats().unwrap().total, model.len() as u64);
        }

        // Final open: stored content is the first-write model, verbatim.
        let last = Storage::open(&db).unwrap();
        prop_assert_eq!(last.schema_version().unwrap(), version);
        assert_model_visible(&last, &model);
    }

    // Two open handles on the same file (same process, WAL mode): a write
    // through one is visible to the other, and dedup keys are shared.
    #[test]
    fn second_connection_shares_data(
        batch in fixtures::batch(16),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vault.db");

        let a = Storage::open(&db).unwrap();
        let b = Storage::open(&db).unwrap();
        for msg in &batch {
            a.insert_message(msg).unwrap();
        }
        let model = fixtures::first_write_model(&batch);
        prop_assert_eq!(b.stats().unwrap().total, model.len() as u64);

        // Same keys through the second handle: all duplicates.
        for msg in &batch {
            prop_assert!(matches!(
                b.insert_message(msg).unwrap(),
                InsertOutcome::Duplicate(_)
            ));
        }
        prop_assert_eq!(a.stats().unwrap().total, model.len() as u64);
        assert_model_visible(&b, &model);
    }
}

/// Every model row is visible through a handle's public API: `stats`
/// counts, and each row is found via a token of its own stored body.
fn assert_model_visible(storage: &Storage, model: &HashMap<(String, String), &NewMessage>) {
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    for ((source, external_id), msg) in model {
        let token = fixtures::tokenize(&msg.body)
            .into_iter()
            .next()
            .or_else(|| fixtures::tokenize(&msg.author).into_iter().next());
        let Some(token) = token else { continue };
        let hits = storage
            .search_filtered(
                &fixtures::quoted_term(&token),
                None,
                None,
                None,
                false,
                10_000,
            )
            .unwrap();
        let hit = hits
            .iter()
            .find(|m| &m.source == source && &m.external_id == external_id)
            .unwrap_or_else(|| panic!("row {source}/{external_id} lost after reopen"));
        assert_eq!(hit.body, msg.body);
    }
}

/// Golden pinned seed: fresh db + golden batch + reopen loop keeps schema
/// version and the exact stored set.
#[test]
fn golden_seed_reopen_preserves_data() {
    let batch = fixtures::golden_batch(0x5EED_0001, 48);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");

    let s = Storage::open(&db).unwrap();
    let version = s.schema_version().unwrap();
    for msg in &batch {
        s.insert_message(msg).unwrap();
    }
    let model = fixtures::first_write_model(&batch);
    drop(s);

    for _ in 0..2 {
        let s = Storage::open(&db).unwrap();
        assert_eq!(s.schema_version().unwrap(), version);
        assert_eq!(s.stats().unwrap().total, model.len() as u64);
    }
}
