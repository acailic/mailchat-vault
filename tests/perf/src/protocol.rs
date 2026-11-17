//! Benchmark protocol: the committed baseline file and the gate math.
//!
//! The CI regression gate does not hardcode its budgets. They live in a
//! committed JSON file ([`Baseline`], default `tests/perf/baseline.json`)
//! together with the methodology they were calibrated under. The file
//! carries a SHA-256 of the canonical methodology JSON: changing the
//! methodology (warmup rule, iteration count, fixture size, seed, …)
//! without recomputing the budgets fails the gate loudly instead of
//! silently comparing numbers measured under different rules.
//!
//! The hash is computed over the compact `serde_json` serialization of
//! the [`Methodology`] struct (field order = declaration order, ASCII
//! strings only), so any JSON tooling can reproduce it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4). Self-contained so the harness adds no dependency;
// `bench_protocol` pins it against the published test vectors.
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 digest of `data` (FIPS 180-4).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    sha256(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// ---------------------------------------------------------------------------
// Baseline file
// ---------------------------------------------------------------------------

/// The measurement rules the budgets were calibrated under. Changing any
/// field changes [`Baseline::methodology_hash`] and invalidates the file.
///
/// Keep every field ASCII with no JSON-escaping characters: the hash is
/// the compact `serde_json` serialization and must be reproducible with
/// any tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Methodology {
    /// Baseline-file schema version.
    pub schema: u32,
    /// Harness name and version that recorded the calibration run.
    pub harness: String,
    /// Fixture message count the budgets apply to.
    pub fixture_count: u64,
    /// Fixture seed (deterministic generator).
    pub seed: u64,
    /// Timed iterations per query.
    pub iterations: u32,
    /// Untimed warmup runs per query.
    pub warmup: u32,
    /// Reference machine class (human-readable pointer to the page).
    pub machine_class: String,
    /// Calibration rule (how budgets sit above measured p95).
    pub calibration: String,
}

/// The committed gate baseline: per-query p95 budgets (milliseconds) plus
/// the methodology they were recorded under and its hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub methodology: Methodology,
    /// SHA-256 (hex) of the canonical methodology JSON; must match
    /// [`Baseline::methodology_hash`] or the file is stale.
    pub methodology_sha256: String,
    /// p95 budget in ms per benchmark query label.
    pub budgets_ms: BTreeMap<String, f64>,
}

/// Everything that can go wrong loading or applying a [`Baseline`].
#[derive(Debug)]
pub enum ProtocolError {
    Io(std::io::Error),
    Parse(String),
    /// Recorded hash does not match the methodology in the file.
    HashMismatch {
        recorded: String,
        computed: String,
    },
    /// The suite has a query the baseline does not budget.
    MissingBudget(String),
    /// The baseline budgets a query the suite does not run.
    UnknownBudget(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Io(e) => write!(f, "baseline file: {e}"),
            ProtocolError::Parse(e) => write!(f, "baseline file parse: {e}"),
            ProtocolError::HashMismatch { recorded, computed } => write!(
                f,
                "baseline methodology hash mismatch: file records {recorded} but its \
                 methodology hashes to {computed}; the methodology changed without \
                 recalibrating the budgets — rerun the calibration (docs/benchmark.md) \
                 and commit both together"
            ),
            ProtocolError::MissingBudget(label) => write!(
                f,
                "baseline has no p95 budget for query {label:?}; add it (or drop the \
                 query) and recalibrate per docs/benchmark.md"
            ),
            ProtocolError::UnknownBudget(label) => write!(
                f,
                "baseline budgets query {label:?} which the suite does not run; remove \
                 the stale entry (docs/benchmark.md)"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl Baseline {
    /// SHA-256 (hex) of the canonical methodology JSON.
    pub fn methodology_hash(methodology: &Methodology) -> String {
        let canonical = serde_json::to_vec(methodology).expect("methodology serializes");
        sha256_hex(&canonical)
    }

    /// Parse and fully verify a baseline: hash match and non-empty budgets.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let baseline: Baseline =
            serde_json::from_slice(bytes).map_err(|e| ProtocolError::Parse(e.to_string()))?;
        let computed = Self::methodology_hash(&baseline.methodology);
        if computed != baseline.methodology_sha256 {
            return Err(ProtocolError::HashMismatch {
                recorded: baseline.methodology_sha256,
                computed,
            });
        }
        if baseline.budgets_ms.is_empty() {
            return Err(ProtocolError::Parse(
                "baseline has no budgets_ms entries".into(),
            ));
        }
        Ok(baseline)
    }

    /// Load and verify a baseline from `path`.
    pub fn load(path: &Path) -> Result<Self, ProtocolError> {
        let bytes = std::fs::read(path).map_err(ProtocolError::Io)?;
        Baseline::from_slice(&bytes)
    }

    /// The p95 budget (ms) for `label`; errors loudly when absent.
    pub fn budget_for(&self, label: &str) -> Result<f64, ProtocolError> {
        self.budgets_ms
            .get(label)
            .copied()
            .ok_or_else(|| ProtocolError::MissingBudget(label.to_string()))
    }

    /// Check the baseline and the query suite describe exactly the same
    /// queries: every suite label budgeted, no stale extras.
    pub fn validate_labels<'a>(
        &self,
        labels: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Vec<ProtocolError>> {
        let suite: Vec<&str> = labels.into_iter().collect();
        let mut errors = Vec::new();
        for label in &suite {
            if let Err(e) = self.budget_for(label) {
                errors.push(e);
            }
        }
        for label in self.budgets_ms.keys() {
            if !suite.contains(&label.as_str()) {
                errors.push(ProtocolError::UnknownBudget(label.clone()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// ---------------------------------------------------------------------------
// Gate math
// ---------------------------------------------------------------------------

/// One queried label's gate decision input.
#[derive(Debug, Clone, PartialEq)]
pub struct GateCheck {
    pub label: String,
    pub p95_ms: f64,
    pub budget_ms: f64,
}

impl GateCheck {
    /// p95 ÷ budget; 1.0 means exactly at the limit.
    pub fn ratio(&self) -> f64 {
        self.p95_ms / self.budget_ms
    }

    /// The gate rule: p95 must stay strictly under its budget.
    pub fn within(&self) -> bool {
        self.p95_ms < self.budget_ms
    }
}

/// Aggregate gate outcome over all checks.
#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    pub passed: bool,
    /// The check closest to (or beyond) its budget by ratio.
    pub worst: GateCheck,
}

/// Evaluate the gate: every check within budget, worst offender reported.
pub fn evaluate_gate(checks: Vec<GateCheck>) -> GateOutcome {
    let worst = checks
        .iter()
        .max_by(|a, b| a.ratio().total_cmp(&b.ratio()))
        .expect("gate requires at least one check")
        .clone();
    GateOutcome {
        passed: checks.iter().all(GateCheck::within),
        worst,
    }
}
