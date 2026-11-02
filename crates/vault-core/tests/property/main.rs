//! Property tests for vault-core's storage contracts.
//!
//! Every property generates random messages with a fixed vocabulary (see
//! [`fixtures`]) and checks a *contract*, not an implementation detail:
//!
//! - [`dedup`]: writes dedup on `(source, external_id)`; the first write of
//!   a key wins forever, and the key is scoped per source.
//! - [`ingest_idempotence`]: re-running [`vault_core::ingest::ingest`] over
//!   the same source never creates a second row and never overwrites stored
//!   content, regardless of order.
//! - [`fts_match`]: FTS5 MATCH results equal an independent oracle of
//!   `unicode61` semantics — unicode (case folding, Latin diacritics,
//!   Cyrillic, CJK runs), prefixes, phrases, and implicit AND.
//! - [`migrations`]: reopening an existing database applies migrations as a
//!   no-op and preserves every stored row.
//! - [`integrity`]: the FTS index stays consistent with the
//!   `archive_messages` table under arbitrary operation sequences, and
//!   recovers via `rebuild_fts`.
//!
//! Determinism and reproduction
//! ----------------------------
//!
//! The generator itself is deterministic: given the same seed it produces
//! the same batches ([`fixtures::golden_batch`]). The `golden_*` tests pin
//! fixed seeds and run on every CI pass.
//!
//! proptest picks a fresh random seed per run and prints it when a property
//! fails, e.g. `Seed: 0x…`. Replay that exact run with:
//!
//! ```text
//! PROPTEST_SEED=0x… cargo test -p vault-core --test property
//! ```
//!
//! Any failing case is also shrunk automatically to a minimal counterexample
//! and persisted under `proptest-regressions/` when it reproduces only
//! intermittently.

mod dedup;
mod fixtures;
mod fts_match;
mod ingest_idempotence;
mod integrity;
mod migrations;

use proptest::prelude::*;

/// Shared proptest config: fewer cases than the 256 default for properties
/// that build a fresh SQLite database per case, which keeps CI snappy while
/// still exercising hundreds of generated states per property.
pub fn prop_config(cases: u32) -> ProptestConfig {
    ProptestConfig::with_cases(cases)
}
