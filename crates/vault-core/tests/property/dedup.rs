//! Dedup contract: writes are keyed on `(source, external_id)` — the first
//! write wins forever, the key is scoped per source, and `stats`/`get`
//! reflect exactly the set of distinct keys ever inserted.

use std::collections::HashMap;

use proptest::collection;
use proptest::prelude::*;

use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::Storage;

use crate::fixtures;

// The stored row for a dedup key equals the *first* message inserted under
// that key; later inserts with the same key return `Duplicate` carrying
// that same row id and change nothing.
proptest! {
    #![proptest_config(crate::prop_config(128))]

    #[test]
    fn first_write_wins_over_arbitrary_sequences(
        batch in fixtures::batch(24),
        mutations in collection::vec(any::<u64>(), 0..=12),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        let mut ids: HashMap<(String, String), i64> = HashMap::new();
        let mut running: HashMap<(String, String), ()> = HashMap::new();
        let model = fixtures::first_write_model(&batch);

        for msg in &batch {
            let outcome = storage.insert_message(msg).unwrap();
            let key = (msg.source.clone(), msg.external_id.clone());
            match outcome {
                InsertOutcome::Inserted(id) => {
                    prop_assert!(!ids.contains_key(&key), "id reused for new key");
                    prop_assert!(ids.values().all(|&other| other != id), "row id duplicated");
                    ids.insert(key.clone(), id);
                    running.insert(key, ());
                }
                InsertOutcome::Duplicate(id) => {
                    let expected = ids.get(&key).copied().unwrap_or_else(|| {
                        panic!("Duplicate returned for unknown key {key:?}")
                    });
                    prop_assert_eq!(id, expected);
                }
            }
            // Invariant checked after EVERY operation: the stored count
            // equals the number of distinct keys seen so far.
            prop_assert_eq!(storage.stats().unwrap().total, running.len() as u64);
        }
        prop_assert_eq!(running.len(), model.len());

        // Re-insert mutated variants of arbitrary already-stored messages:
        // every one must be a no-op.
        for mask in mutations {
            let Some((_, first)) = model.iter().nth(mask as usize % model.len().max(1)) else {
                break;
            };
            let again = fixtures::mutate(first);
            let outcome = storage.insert_message(&again).unwrap();
            match outcome {
                InsertOutcome::Duplicate(id) => {
                    prop_assert_eq!(Some(id), ids.get(&(again.source.clone(), again.external_id.clone())).copied());
                }
                other => panic!("re-insert created a second row: {other:?}"),
            }
            prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
        }

        // Final state: every model row is stored verbatim.
        for ((source, external_id), first) in &model {
            let id = ids[&(source.clone(), external_id.clone())];
            let stored = storage.get(id).unwrap().expect("model row missing");
            prop_assert_eq!(stored.source, source.as_str());
            prop_assert_eq!(stored.external_id, external_id.as_str());
            prop_assert_eq!(stored.author, first.author.as_str());
            prop_assert_eq!(stored.sent_at, first.sent_at.as_str());
            prop_assert_eq!(stored.body, first.body.as_str());
        }
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    }

    // The same external id under different sources is one row per source;
    // none of them collide across namespaces.
    #[test]
    fn key_is_scoped_per_source(
        external_id in fixtures::external_id(),
        sources in collection::vec(proptest::sample::select(fixtures::SOURCES), 1..=6),
        body in fixtures::field_text(1, 4),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        let mut distinct: Vec<&str> = Vec::new();
        for s in &sources {
            if !distinct.contains(s) {
                distinct.push(s);
            }
        }

        for (i, source) in distinct.iter().enumerate() {
            let msg = NewMessage::new(*source, external_id.clone(), "ana", "", format!("{body} v{i}"));
            prop_assert!(storage.insert_message(&msg).unwrap().was_inserted());
        }
        prop_assert_eq!(storage.stats().unwrap().total, distinct.len() as u64);

        // Re-insert each with different content: all duplicates, no change.
        for source in &distinct {
            let msg = NewMessage::new(*source, external_id.clone(), "marko", "", "different body");
            prop_assert!(!storage.insert_message(&msg).unwrap().was_inserted());
        }
        prop_assert_eq!(storage.stats().unwrap().total, distinct.len() as u64);
    }
}

/// Golden pinned seed: the same deterministic batch always exercises the
/// same dedup paths (regression anchor for reproduction).
#[test]
fn golden_seed_first_write_wins() {
    let batch = fixtures::golden_batch(0xD1CE_BEEF, 64);
    assert!(
        fixtures::first_write_model(&batch).len() < batch.len(),
        "golden batch must contain duplicate keys"
    );

    let storage = Storage::open_in_memory().unwrap();
    for msg in &batch {
        storage.insert_message(msg).unwrap();
    }
    let model = fixtures::first_write_model(&batch);
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);

    for msg in &batch {
        storage.insert_message(&fixtures::mutate(msg)).unwrap();
    }
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    assert!(
        storage
            .search(&fixtures::quoted_term("mutated-variant"), 1000)
            .unwrap()
            .is_empty(),
        "mutated content must never be searchable"
    );
}
