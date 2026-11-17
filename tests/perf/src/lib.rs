//! `vault-perf` library target: the benchmark harness logic exposed for
//! the `bench_protocol` integration tests. `src/main.rs` is the CLI and
//! contains only orchestration; everything testable (fixture generation,
//! statistics, suite, baseline/gate protocol) lives here.

pub mod fixture;
pub mod measure;
pub mod protocol;
pub mod suite;
