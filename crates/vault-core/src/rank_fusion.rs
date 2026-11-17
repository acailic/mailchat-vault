//! Reciprocal Rank Fusion: a pure, I/O-free rank-list combiner.
//!
//! # Design (and why)
//!
//! The Ask pipeline ranks each query with two incomparable scorers — FTS5
//! bm25 relevance and cosine similarity over local embeddings — and BEIR
//! (Thakur et al., NeurIPS 2021) shows lexical retrieval beats zero-shot
//! dense retrieval on many out-of-domain corpora, while a personal archive
//! is maximally out-of-domain for any embedder. Fusing *both* ranked lists
//! rather than trusting either score scale is therefore the evidence-backed
//! choice, and rank-based fusion needs no calibration between the two.
//!
//! The fuse is [Reciprocal Rank Fusion](RRF) (Cormack, Buettner & Clarke,
//! "Reciprocal Rank Fusion outperforms Condorcet and individual Rank
//! Learning Methods", SIGIR 2009, doi:10.1145/1571941.1572114): score(d) =
//! Σ_lists 1/(RRF_K + rank). RRF beat the best individual system and
//! learned fusion in that paper — with zero training data.
//!
//! Determinism: each id's score is accumulated in list order (fixed), and
//! ties order by ascending id, so the same inputs always produce the same
//! output (see [`rrf_fuse_n`]).
//!
//! [RRF]: https://dl.acm.org/doi/10.1145/1571941.1572114

use std::collections::HashMap;

/// The standard RRF smoothing constant, k = 60: score(d) =
/// Σ_lists 1/(RRF_K + rank), rank starting at 1. The value comes from
/// Cormack, Buettner & Clarke, "Reciprocal Rank Fusion outperforms
/// Condorcet and individual Rank Learning Methods", SIGIR 2009
/// (doi:10.1145/1571941.1572114), where it outperformed both Condorcet
/// fusion and learned fusion.
pub const RRF_K: u32 = 60;

/// Fuse N ranked id lists (each best first) with Reciprocal Rank Fusion:
/// score(d) = Σ_lists 1/(RRF_K + rank), rank starting at 1.
///
/// Deterministic: each id's score is the sum of its reciprocal ranks in
/// list order, and equal scores order by ascending id, so the output is
/// reproducible run to run. Returns at most `limit` ids, best first.
/// Rank-based fusion sidesteps the incomparable score scales of FTS5
/// relevance and cosine similarity.
pub fn rrf_fuse_n(lists: &[&[i64]], limit: usize) -> Vec<i64> {
    let mut scores: HashMap<i64, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (RRF_K + rank as u32 + 1) as f32;
        }
    }
    let mut ranked: Vec<(i64, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|(id, _)| id).collect()
}

/// Fuse two ranked id lists (best first) — the Ask pipeline's hybrid entry
/// point over the FTS leg and the vector leg. See [`rrf_fuse_n`] for the
/// general N-list form and the fusion rationale.
pub fn rrf_fuse(fts: &[i64], vector: &[i64], limit: usize) -> Vec<i64> {
    rrf_fuse_n(&[fts, vector], limit)
}
