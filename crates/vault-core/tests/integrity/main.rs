//! Integrity and durability suite for the vault database: corruption
//! detection and recovery, WAL crash scenarios, export/import round-trips,
//! cross-source dedup, VACUUM/ANALYZE on large databases, and schema
//! migrations over databases from previous versions — including the M1.4
//! safe-migration path (snapshot, preflight, rollback, refusal).
//!
//! These tests deliberately reach below `Storage` with a raw `rusqlite`
//! connection to set up failure states (torn files, half-applied schemas,
//! bulk deletes) that the application itself never produces — then assert
//! the storage layer's behavior when it meets them.

mod canonical;
mod conversation_keys;
mod corruption;
mod dedup;
mod migrations;
mod roundtrip;
mod safe_migration;
mod util;
mod vacuum;
mod wal_crash;
