//! Schema/index integrity: under arbitrary sequences of inserts (new keys,
//! duplicate keys with different content) and searches, the `messages`
//! table and the FTS index stay consistent — checked purely through the
//! public API: `stats` matches the model, search hits always agree with the
//! oracle and contain only known rows, and rebuilding a healthy index
//! changes nothing. After an index wipe, `rebuild_fts` restores exact
//! agreement with the table.

use std::collections::{BTreeSet, HashMap};

use proptest::collection;
use proptest::prelude::*;

use vault_core::message::NewMessage;
use vault_core::storage::Storage;

use crate::fixtures::{self, ASCII_WORDS};

/// Sampled probes run after every operation phase: one guaranteed-absent
/// token plus the whole small ASCII vocabulary.
fn probe_terms() -> Vec<&'static str> {
    let mut terms: Vec<&'static str> = ASCII_WORDS.to_vec();
    terms.push("zzzznotpresent");
    terms
}

/// For every probe term, search hits must exactly equal the oracle's
/// expected set over the model, and every hit must be a known row.
fn check_probes(storage: &Storage, model: &HashMap<(String, String), &NewMessage>) {
    let limit = model.len() as u32 + 8;
    for term in probe_terms() {
        let expected: BTreeSet<_> = model
            .values()
            .filter(|m| fixtures::term_matches(m, term))
            .map(|m| (m.source.clone(), m.external_id.clone()))
            .collect();
        let hits: BTreeSet<_> = storage
            .search_filtered(&fixtures::quoted_term(term), None, None, None, false, limit)
            .unwrap()
            .into_iter()
            .map(|m| (m.source, m.external_id))
            .collect();
        assert_eq!(hits, expected, "probe term: {term}");
        for key in &hits {
            assert!(model.contains_key(key), "hit for unknown row: {key:?}");
        }
    }
}

proptest! {
    #![proptest_config(crate::prop_config(64))]

    // After EVERY operation in a random sequence the database satisfies
    // its invariants; at the end, a rebuild of the healthy index is a
    // no-op, and wipe + rebuild restores exact agreement.
    #[test]
    fn index_stays_consistent_under_random_ops(
        ops in fixtures::batch(24),
        reinserts in collection::vec(any::<u64>(), 0..=12),
    ) {
        let storage = Storage::open_in_memory().unwrap();
        let mut model: HashMap<(String, String), NewMessage> = HashMap::new();

        // Phase 1: fresh inserts, invariants checked after each one.
        for (i, msg) in ops.iter().enumerate() {
            storage.insert_message(msg).unwrap();
            model
                .entry((msg.source.clone(), msg.external_id.clone()))
                .or_insert_with(|| msg.clone());
            prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
            if i % 6 == 5 {
                let snapshot: HashMap<_, _> =
                    model.iter().map(|(k, v)| (k.clone(), v)).collect();
                check_probes(&storage, &snapshot);
            }
        }

        // Phase 2: duplicate-key rewrites with different content.
        for mask in &reinserts {
            if model.is_empty() {
                break;
            }
            let keys: Vec<_> = model.keys().cloned().collect();
            let key = &keys[(*mask as usize) % keys.len()];
            let mutated = {
                let first = &model[key];
                let mut m = first.clone();
                m.body = format!("mutated-variant {}", first.body);
                m.author = format!("mutated-variant {}", first.author);
                m
            };
            storage.insert_message(&mutated).unwrap();
            prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
        }
        let snapshot: HashMap<_, _> = model.iter().map(|(k, v)| (k.clone(), v)).collect();
        check_probes(&storage, &snapshot);

        // Phase 3: rebuilding a healthy index changes nothing observable.
        let before: Vec<BTreeSet<_>> = probe_terms()
            .iter()
            .map(|t| {
                storage
                    .search_filtered(&fixtures::quoted_term(t), None, None, None, false, model.len() as u32 + 8)
                    .unwrap()
                    .into_iter()
                    .map(|m| (m.source, m.external_id))
                    .collect()
            })
            .collect();
        storage.rebuild_fts().unwrap();
        for (i, term) in probe_terms().iter().enumerate() {
            let after: BTreeSet<_> = storage
                .search_filtered(&fixtures::quoted_term(term), None, None, None, false, model.len() as u32 + 8)
                .unwrap()
                .into_iter()
                .map(|m| (m.source, m.external_id))
                .collect();
            prop_assert_eq!(&before[i], &after, "rebuild changed results for {}", term);
        }
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);

        // Phase 4: wipe the index (simulated corruption) — the table is
        // intact (stats/get), searches see nothing; rebuild restores exact
        // oracle agreement.
        storage.wipe_fts_for_tests().unwrap();
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
        for term in probe_terms() {
            let hits = storage
                .search_filtered(&fixtures::quoted_term(term), None, None, None, false, model.len() as u32 + 8)
                .unwrap();
            prop_assert!(hits.is_empty(), "wiped index must match nothing ({})", term);
        }
        storage.rebuild_fts().unwrap();
        check_probes(&storage, &snapshot);
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    }
}

/// Golden pinned seed: the deterministic golden batch passes every
/// integrity probe, survives a wipe + rebuild cycle, and the schema version
/// is stable throughout.
#[test]
fn golden_seed_integrity_with_recovery() {
    let batch = fixtures::golden_batch(0x000A_B1DE, 48);
    let storage = Storage::open_in_memory().unwrap();
    let version = storage.schema_version().unwrap();
    for msg in &batch {
        storage.insert_message(msg).unwrap();
    }
    let model: HashMap<_, _> = fixtures::first_write_model(&batch)
        .into_iter()
        .map(|(k, v)| (k, v.clone()))
        .collect();
    let snapshot: HashMap<_, _> = model.iter().map(|(k, v)| (k.clone(), v)).collect();

    check_probes(&storage, &snapshot);
    assert_eq!(storage.schema_version().unwrap(), version);

    storage.wipe_fts_for_tests().unwrap();
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    storage.rebuild_fts().unwrap();
    check_probes(&storage, &snapshot);
    assert_eq!(storage.schema_version().unwrap(), version);
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);
}
