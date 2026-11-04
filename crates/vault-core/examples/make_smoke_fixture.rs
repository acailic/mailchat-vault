//! Generates the frozen Windows-upgrade fixture database
//! (`tests/windows-smoke/fixtures/vault-v0.1.0.db`).
//!
//! The Windows smoke test installs the **old** (v0.1.0) app, seeds its
//! data directory with this fixture, then upgrades over it and asserts
//! the archive survived — so the fixture must stay frozen at the
//! v0.1.0 schema and demo content (schema 11; the demo messages are
//! unchanged since). The fixture was generated from the **v0.1.0 tag**;
//! this example refuses to emit anything else, so accidentally running
//! it against a newer workspace fails loudly instead of silently moving
//! the fixture forward (the old app refuses newer-schema databases by
//! design — see `safe_migration`).
//!
//! To regenerate (only ever from the `v0.1.0` tag):
//!
//! ```sh
//! git archive v0.1.0 | tar -x -C /tmp/v010
//! cp crates/vault-core/examples/make_smoke_fixture.rs /tmp/v010/crates/vault-core/examples/
//! ( cd /tmp/v010/crates/vault-core && cargo run --release --example make_smoke_fixture -- \
//!     <repo>/tests/windows-smoke/fixtures/vault-fresh.db )
//! mv <repo>/tests/windows-smoke/fixtures/vault-fresh.db \
//!    <repo>/tests/windows-smoke/fixtures/vault-v0.1.0.db
//! ```
//!
//! The written file is WAL-checkpointed (no `-wal`/`-shm` sidecars) so
//! the smoke test can copy exactly one file. `crates/vault-core/tests/
//! smoke_fixture.rs` pins the fixture's contents against drift.

use vault_core::demo;
use vault_core::ingest::{self, Collected, Source};
use vault_core::storage::Storage;

/// Schema version the v0.1.0 binaries ship (migration 11). The fixture
/// must land exactly here: newer is refused by the old app, older would
/// not exercise the upgrade path we want to pin.
const V0_1_0_SCHEMA: i64 = 11;

/// The demo messages as a simple whole-archive source — the same data
/// the app's **Load demo** button seeds, through the same idempotent
/// ingest path (re-running never duplicates).
struct DemoSource;

impl Source for DemoSource {
    fn name(&self) -> &str {
        demo::DEMO_SOURCE
    }
    fn collect(&self) -> vault_core::Result<Collected> {
        Ok(Collected {
            messages: demo::messages(),
            failed: 0,
        })
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "vault-v0.1.0.db".to_string());
    let path = std::path::Path::new(&out);
    if path.exists() {
        eprintln!("make_smoke_fixture: refusing to overwrite {out}");
        std::process::exit(2);
    }
    let storage = Storage::open(&out).unwrap_or_else(|e| panic!("opening {out}: {e}"));
    let summary = ingest::ingest(&storage, &DemoSource)
        .unwrap_or_else(|e| panic!("seeding demo messages: {e}"));
    drop(storage); // clean close: WAL checkpointed, sidecars removed

    // Fold any leftover WAL content into the main file and validate the
    // frozen-shape contract before handing the fixture to the smoke test.
    let conn = rusqlite::Connection::open(&out).unwrap_or_else(|e| panic!("reopening {out}: {e}"));
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap_or_else(|e| panic!("checkpointing {out}: {e}"));
    let schema: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap_or_else(|e| panic!("reading schema version: {e}"));
    drop(conn);
    if schema != V0_1_0_SCHEMA {
        eprintln!(
            "make_smoke_fixture: generated schema {schema}, expected \
             {V0_1_0_SCHEMA} (the v0.1.0 schema). Regenerate from the \
             v0.1.0 tag; refusing to leave {out} behind."
        );
        let _ = std::fs::remove_file(&out);
        std::process::exit(1);
    }
    println!(
        "wrote {out}: schema={schema} source={} inserted={} skipped={}",
        summary.source, summary.inserted, summary.skipped
    );
    for sidecar in ["-wal", "-shm"] {
        let p = format!("{out}{sidecar}");
        if std::path::Path::new(&p).exists() {
            eprintln!("warning: {p} still exists after checkpoint");
        }
    }
}
