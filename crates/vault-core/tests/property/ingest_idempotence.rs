//! Idempotent ingest contract: running [`vault_core::ingest::ingest`] over
//! a source a second (or n-th) time inserts nothing, skips everything as
//! duplicates, never overwrites stored content, and reports
//! `seen == inserted + skipped + failed`. The stored set is independent of
//! the order messages arrive in.

use std::collections::HashMap;

use proptest::collection;
use proptest::prelude::*;

use vault_core::ingest::ingest;
use vault_core::message::NewMessage;
use vault_core::storage::Storage;

use crate::fixtures::{self, FixtureSource};

// Ingest the same source twice (second pass with visibly different
// content): the second pass is a complete no-op on storage.
proptest! {
    #![proptest_config(crate::prop_config(96))]

    #[test]
    fn reingest_is_a_noop(batch in fixtures::batch(30)) {
        let storage = Storage::open_in_memory().unwrap();
        let model = fixtures::first_write_model(&batch);

        let pass1 = ingest(&storage, &FixtureSource::new("gen", batch.clone(), 0)).unwrap();
        prop_assert_eq!(pass1.source, "gen");
        prop_assert_eq!(pass1.seen, batch.len() as u64);
        prop_assert_eq!(pass1.failed, 0);
        prop_assert_eq!(pass1.inserted, model.len() as u64);
        prop_assert_eq!(pass1.skipped, batch.len() as u64 - model.len() as u64);
        prop_assert_eq!(
            pass1.seen,
            pass1.inserted + pass1.skipped + pass1.failed
        );

        // Second pass: same keys, different content. Nothing may change.
        let replay: Vec<_> = batch.iter().map(fixtures::mutate).collect();
        let pass2 = ingest(&storage, &FixtureSource::new("gen", replay, 0)).unwrap();
        prop_assert_eq!(pass2.inserted, 0);
        prop_assert_eq!(pass2.skipped, pass2.seen);
        prop_assert_eq!(pass2.seen, batch.len() as u64);
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);

        // Third pass: the ORIGINAL batch again — still nothing.
        let pass3 = ingest(&storage, &FixtureSource::new("gen", batch.clone(), 0)).unwrap();
        prop_assert_eq!(pass3.inserted, 0);
        prop_assert_eq!(pass3.skipped, pass3.seen);

        // Content still equals the first-pass model; mutated bodies are
        // nowhere in the index.
        assert_stored_set(&storage, &model);
        prop_assert!(
            storage
                .search_filtered(&fixtures::quoted_term("mutated-variant"), None, None, 1000)
                .unwrap()
                .is_empty()
        );
    }

    // The stored set does not depend on the order messages arrive in: for
    // every permutation, the stored rows are exactly that permutation's
    // first-write model (the key set is identical; which duplicate wins
    // follows the ingest order, which is itself the contract).
    #[test]
    fn ingest_is_order_independent(
        batch in fixtures::batch(20),
        shifts in collection::vec(any::<u64>(), 1..=4),
    ) {
        let keys = fixtures::first_write_model(&batch).len();
        prop_assume!(!batch.is_empty());

        for shift in shifts {
            // One deterministic rotation per shift value.
            let len = batch.len().max(1);
            let permuted: Vec<_> = (0..len)
                .map(|i| batch[(i + shift as usize) % len].clone())
                .collect();
            let model = fixtures::first_write_model(&permuted);

            let storage = Storage::open_in_memory().unwrap();
            ingest(&storage, &FixtureSource::new("gen", permuted.clone(), 0)).unwrap();
            prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
            prop_assert_eq!(model.len(), keys, "distinct keys must not depend on order");
            assert_stored_set(&storage, &model);
        }
    }

    // `seen == inserted + skipped + failed` also holds when the source
    // reports unparseable messages.
    #[test]
    fn summary_accounts_for_parse_failures(
        batch in fixtures::batch(12),
        failed in 0u64..=5,
    ) {
        let seen_total = batch.len() as u64;
        let model = fixtures::first_write_model(&batch);
        let storage = Storage::open_in_memory().unwrap();
        let summary = ingest(&storage, &FixtureSource::new("gen", batch.clone(), failed)).unwrap();
        prop_assert_eq!(summary.failed, failed);
        prop_assert_eq!(summary.seen, seen_total + failed);
        prop_assert_eq!(summary.inserted, model.len() as u64);
        prop_assert_eq!(summary.skipped, seen_total - model.len() as u64);
        prop_assert_eq!(
            summary.seen,
            summary.inserted + summary.skipped + summary.failed
        );
        prop_assert_eq!(storage.stats().unwrap().total, model.len() as u64);
    }
}

/// Every model row is retrievable through the index and verbatim: search a
/// token from its own body/author and the row must come back with the
/// first-pass content. Rows without any token are unsearchable by
/// definition and skipped.
fn assert_stored_set(storage: &Storage, model: &HashMap<(String, String), &NewMessage>) {
    let mut seen = std::collections::HashSet::new();
    let mut token_bearing = 0usize;
    for ((source, external_id), msg) in model {
        // Find the row through the index: search a token from its own body.
        let token = fixtures::tokenize(&msg.body)
            .into_iter()
            .next()
            .or_else(|| fixtures::tokenize(&msg.author).into_iter().next());
        let Some(token) = token else {
            continue;
        };
        token_bearing += 1;
        let hits = storage
            .search_filtered(&fixtures::quoted_term(&token), None, None, 10_000)
            .unwrap();
        let hit = hits
            .iter()
            .find(|m| m.source == *source && m.external_id == *external_id)
            .unwrap_or_else(|| panic!("row {source}/{external_id} not found via token {token}"));
        assert_eq!(hit.body, msg.body);
        assert_eq!(hit.author, msg.author);
        assert_eq!(hit.sent_at, msg.sent_at);
        seen.insert((source.clone(), external_id.clone()));
    }
    assert_eq!(seen.len(), token_bearing);
}

/// Golden pinned seed: a fixed deterministic batch re-ingested stays at the
/// model size and keeps first-pass content.
#[test]
fn golden_seed_reingest_is_a_noop() {
    let batch = fixtures::golden_batch(0xC0FFEE, 64);
    let storage = Storage::open_in_memory().unwrap();
    let model = fixtures::first_write_model(&batch);

    let pass1 = ingest(&storage, &FixtureSource::new("golden", batch.clone(), 0)).unwrap();
    assert_eq!(pass1.inserted, model.len() as u64);
    let pass2 = ingest(&storage, &FixtureSource::new("golden", batch.clone(), 0)).unwrap();
    assert_eq!((pass2.inserted, pass2.skipped), (0, batch.len() as u64));
    assert_eq!(storage.stats().unwrap().total, model.len() as u64);
}
