//! Local embeddings and vector search: the semantic half of hybrid ranking.
//!
//! # Design (and why)
//!
//! **Embedding model.** The shipped model is [`HashEmbedder`], a
//! dependency-free, deterministic feature-hashing text embedder (word
//! unigrams + word bigrams + character 3-grams hashed into signed buckets,
//! L2-normalized). The alternatives — `fastembed` or a GGUF mini model —
//! were ruled out for now because both require model weights that either
//! download at first run (breaking the "completely offline" promise) or
//! must be bundled into the repo as a multi-megabyte binary; `fastembed`
//! additionally pulls in native ONNX runtime build steps. The
//! [`Embedder`] trait plus the model-id-versioned embedding rows keep a
//! future bundled model a drop-in: it ships a new `id`, the incremental
//! job re-embeds, old vectors are simply replaced.
//!
//! **Vector index.** Chosen the simple way, per spec: a plain SQLite table
//! with **one i8-quantized byte per component per message** (512 bytes at
//! [`HashEmbedder::DIM`]) and a **manual cosine scan** in Rust — vectors
//! are stored normalized and quantized, queries are quantized the same
//! way, and the cosine is an integer dot product (fast even in debug
//! builds). [`sqlite-vec`] was considered and rejected: it adds a C
//! extension dependency for what is, at personal-archive scale (≈100k
//! messages ≈ 50 MB of vectors), a linear scan measured at ~75 ms. The
//! budgets (job / scan / hybrid ask over 100k) are enforced in
//! `tests/vector_perf.rs`.
//!
//! **Hybrid ranking.** FTS5 relevance and cosine similarity have
//! incomparable score scales, so the two ranked lists are fused with
//! Reciprocal Rank Fusion ([`rrf_fuse`]): score(d) = Σ 1/(k + rank),
//! k = 60 (the TREC default). Rank-based fusion needs no calibration and
//! is order-deterministic (ties break by message id).
//!
//! **Incrementality.** The embedding job ([`run_embedding_job`]) embeds
//! only messages lacking a row for the *current* model id — new imports
//! after the first run cost proportional to their size, and re-running is
//! a no-op.
//!
//! [`sqlite-vec`]: https://github.com/asg017/sqlite-vec

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::storage::Storage;
use crate::Result;

/// Characters of each body fed to the embedder (bounded cost; long tails
/// barely move a similarity score, and FTS still covers the full body).
pub const EMBED_BODY_PREFIX_CHARS: usize = 2_000;
/// How many messages one embedding-job pass writes per transaction.
const JOB_BATCH: u64 = 500;

/// Scan floor: vector hits at or below this cosine similarity are dropped.
/// Measured on the fixture families of `tests/hybrid_ask.rs` and
/// `tests/vector_search.rs`: unrelated pairs land at ≲ 0.06 (collision
/// noise at 512 buckets) while related pairs — even subword-only ones —
/// score ≥ 0.14, so 0.08 separates the two populations with margin.
pub const MIN_COSINE: f32 = 0.08;

/// Quantization scale: stored vectors are i8 with q = round(SCALE · x)
/// where x is a component of a unit vector, so |q| ≤ 127 fits i8.
pub const I8_SCALE: f32 = 127.0;

/// Quantize a (normalized) vector to one i8 byte per component.
pub fn quantize_i8(v: &[f32]) -> Vec<u8> {
    v.iter()
        .map(|x| (x.clamp(-1.0, 1.0) * I8_SCALE).round() as i8 as u8)
        .collect()
}

/// Quantize a query the same way, for the integer dot product at scan time.
pub fn quantize_query_i8(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x.clamp(-1.0, 1.0) * I8_SCALE).round() as i8)
        .collect()
}

// ---------------------------------------------------------------------------
// Embedder
// ---------------------------------------------------------------------------

/// A local text embedding backend: pure, deterministic, offline.
pub trait Embedder: Send + Sync {
    /// Stable model identity stored with every embedding row; changing it
    /// (new model, changed dim) invalidates all stored vectors.
    fn id(&self) -> &str;
    /// Dimensionality of [`Embedder::embed`] output.
    fn dim(&self) -> u16;
    /// Embed `text` into a unit-length vector of `dim` floats.
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// The default local embedding model: signed feature hashing over word
/// unigrams, word bigrams, and character 3-grams of each padded token.
///
/// Deterministic across runs and platforms (FNV-1a over UTF-8 feature
/// strings, fixed accumulation order), so incremental jobs and re-embeds
/// are stable. Similarity behavior: texts sharing vocabulary or subwords
/// score high; unrelated texts score near zero (hashing collisions only).
#[derive(Debug, Clone, Copy, Default)]
pub struct HashEmbedder;

impl HashEmbedder {
    pub const MODEL_ID: &'static str = "hash-ngram-v1";
    /// 512 buckets: at personal-archive scale this keeps unrelated-text
    /// collision noise well under [`MIN_COSINE`] (measured ≲ 0.06) while
    /// related pairs score 0.15–0.55 — and with i8 storage the index is
    /// still only 512 B per message.
    pub const DIM: u16 = 512;

    pub fn new() -> Self {
        Self
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &str {
        Self::MODEL_ID
    }

    fn dim(&self) -> u16 {
        Self::DIM
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let tokens = tokenize(text);
        let mut v = vec![0.0_f32; Self::DIM as usize];
        // Feature bytes are assembled in reusable buffers — no per-feature
        // allocation on the embedding-job hot path.
        let mut feature: Vec<u8> = Vec::with_capacity(16);
        let mut padded: Vec<char> = Vec::with_capacity(32);
        let mut add = |feature: &[u8], weight: f32| {
            let h = fnv1a64(feature);
            let bucket = (h % Self::DIM as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { 1.0 } else { -1.0 };
            v[bucket] += sign * weight;
        };
        for window in tokens.windows(2) {
            feature.clear();
            feature.extend_from_slice(b"b:");
            feature.extend_from_slice(window[0].as_bytes());
            feature.push(b'|');
            feature.extend_from_slice(window[1].as_bytes());
            feature.push(b'|');
            add(&feature, 1.0);
        }
        for token in &tokens {
            feature.clear();
            feature.extend_from_slice(b"w:");
            feature.extend_from_slice(token.as_bytes());
            add(&feature, 1.0);
            // Character 3-grams of the boundary-padded token give subword
            // overlap (morphology, typos) that exact FTS tokens miss. The
            // padded chars are collected into a reusable buffer and each
            // gram written through a stack scratch — no heap allocation.
            padded.clear();
            padded.push('\u{2}');
            padded.extend(token.chars());
            padded.push('\u{3}');
            for gram in padded.windows(3) {
                feature.clear();
                feature.extend_from_slice(b"c:");
                let mut scratch = [0u8; 4];
                for ch in gram {
                    feature.extend_from_slice(ch.encode_utf8(&mut scratch).as_bytes());
                }
                add(&feature, 0.4);
            }
        }
        l2_normalize(&mut v);
        v
    }
}

/// Lowercased alphanumeric tokens (Unicode-aware); everything else splits.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            tokens.push(std::mem::take(current));
        }
    };
    for c in text.chars() {
        let lowered: String = c.to_lowercase().collect();
        for lc in lowered.chars() {
            if lc.is_alphanumeric() {
                current.push(lc);
            } else {
                flush(&mut current, &mut tokens);
            }
        }
    }
    flush(&mut current, &mut tokens);
    tokens
}

/// FNV-1a 64-bit over bytes: small, fast, and — unlike `DefaultHasher` —
/// with a documented, stable definition we control across Rust versions.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Scale to unit length in place (a zero vector stays zero).
fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity of two equal-length vectors (normalized storage makes
/// this the dot product); `None` on length mismatch.
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).map(|(x, y)| x * y).sum())
}

// ---------------------------------------------------------------------------
// Embedding job (incremental) and status
// ---------------------------------------------------------------------------

/// One vector-search hit: the message id and its cosine score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorHit {
    pub message_id: i64,
    pub score: f32,
}

/// What one embedding-job run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedJobSummary {
    pub model: String,
    pub dim: u16,
    /// Messages embedded by this run.
    pub embedded: u64,
    /// Messages still lacking a current-model embedding afterwards.
    pub remaining: u64,
    pub elapsed_ms: u64,
}

/// Embedding coverage for the UI and the hybrid-ranking fallback decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingStatus {
    pub model: String,
    pub dim: u16,
    /// Messages with a current-model embedding.
    pub embedded: u64,
    /// Total messages in the archive.
    pub messages: u64,
    /// `messages - embedded` (never negative).
    pub pending: u64,
}

/// Embedding coverage of `embedder` over the archive in `storage`.
pub fn embedding_status(storage: &Storage, embedder: &dyn Embedder) -> Result<EmbeddingStatus> {
    let counts = storage.embedding_counts(embedder.id())?;
    Ok(EmbeddingStatus {
        model: embedder.id().to_string(),
        dim: embedder.dim(),
        embedded: counts.embedded,
        pending: counts.messages.saturating_sub(counts.embedded),
        messages: counts.messages,
    })
}

/// Embed every message that lacks a current-model embedding, in batches,
/// each batch in one transaction. `max` caps how many messages this run
/// may embed (`None` = all). Re-running over an up-to-date archive is a
/// no-op that embeds nothing.
pub fn run_embedding_job(
    storage: &Storage,
    embedder: &dyn Embedder,
    max: Option<u64>,
) -> Result<EmbedJobSummary> {
    let start = Instant::now();
    let mut embedded: u64 = 0;
    loop {
        let remaining_budget = max.map(|m| m.saturating_sub(embedded));
        let batch = JOB_BATCH.min(remaining_budget.unwrap_or(JOB_BATCH));
        if batch == 0 {
            break;
        }
        let pending = storage.pending_embeddings(embedder.id(), batch)?;
        if pending.is_empty() {
            break;
        }
        let items: Vec<(i64, Vec<f32>)> = pending
            .iter()
            .map(|(id, body)| {
                let prefix: String = body.chars().take(EMBED_BODY_PREFIX_CHARS).collect();
                (*id, embedder.embed(&prefix))
            })
            .collect();
        let wrote = items.len();
        storage.embed_messages_batch(embedder.id(), embedder.dim(), &items)?;
        embedded += wrote as u64;
        if wrote < batch as usize {
            break;
        }
    }
    let status = embedding_status(storage, embedder)?;
    Ok(EmbedJobSummary {
        model: embedder.id().to_string(),
        dim: embedder.dim(),
        embedded,
        remaining: status.pending,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Reciprocal Rank Fusion
// ---------------------------------------------------------------------------

/// The standard RRF smoothing constant (TREC default).
pub const RRF_K: u32 = 60;

/// Fuse two ranked id lists (best first) with Reciprocal Rank Fusion:
/// score(d) = Σ_lists 1/(RRF_K + rank), rank starting at 1. Deterministic:
/// equal scores order by ascending id. Returns at most `limit` ids, best
/// first. Rank-based fusion sidesteps the incomparable score scales of
/// FTS5 relevance and cosine similarity.
pub fn rrf_fuse(fts: &[i64], vector: &[i64], limit: usize) -> Vec<i64> {
    let mut scores: HashMap<i64, f32> = HashMap::new();
    for (rank, id) in fts.iter().enumerate() {
        *scores.entry(*id).or_insert(0.0) += 1.0 / (RRF_K + rank as u32 + 1) as f32;
    }
    for (rank, id) in vector.iter().enumerate() {
        *scores.entry(*id).or_insert(0.0) += 1.0 / (RRF_K + rank as u32 + 1) as f32;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedder_is_deterministic_and_normalized() {
        let e = HashEmbedder::new();
        let a = e.embed("quarterly vault report");
        let b = e.embed("quarterly vault report");
        assert_eq!(a, b, "same input, same vector");
        assert_eq!(a.len(), HashEmbedder::DIM as usize);
        let norm = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit length, got {norm}");
        assert!(a.iter().any(|x| *x != 0.0), "not the zero vector");
    }

    #[test]
    fn related_texts_score_above_unrelated() {
        let e = HashEmbedder::new();
        let q = e.embed("vault sync problems");
        let related = e.embed("problems with syncing the vault again");
        let unrelated = e.embed("lunch tomorrow at the usual place");
        let r = cosine(&q, &related).expect("same dim");
        let u = cosine(&q, &unrelated).expect("same dim");
        assert!(r > u, "related ({r}) must beat unrelated ({u})");
        assert!(r > 0.2, "related texts share substantial features ({r})");
        assert!(u.abs() < 0.1, "unrelated texts stay near zero ({u})");
    }

    #[test]
    fn subword_overlap_scores_above_unrelated() {
        // No shared whole word with the query; only char 3-grams overlap.
        let e = HashEmbedder::new();
        let q = e.embed("deploying");
        let variant = e.embed("redeployments");
        let unrelated = e.embed("kiwifruit");
        assert!(cosine(&q, &variant).unwrap() > cosine(&q, &unrelated).unwrap());
    }

    #[test]
    fn tokenizer_splits_on_non_alphanumerics() {
        assert_eq!(
            tokenize("Hello, WORLD! it's 2026"),
            vec!["hello", "world", "it", "s", "2026"]
        );
        assert!(tokenize("···///").is_empty());
    }

    #[test]
    fn cosine_rejects_length_mismatch() {
        assert!(cosine(&[1.0, 0.0], &[1.0]).is_none());
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), Some(0.0));
    }

    #[test]
    fn quantization_round_trips_within_a_few_percent() {
        let e = HashEmbedder::new();
        let v = e.embed("vault sync problems");
        let q = quantize_i8(&v);
        assert_eq!(q.len(), HashEmbedder::DIM as usize);
        // Known values quantize exactly (clamped unit components, scale 127).
        assert_eq!(quantize_i8(&[1.0, -1.0, 0.0, 0.5]), vec![127u8, 129, 0, 64]);
        // Dequantized cosine against the original stays ≈ 1.
        let deq: Vec<f32> = q.iter().map(|b| *b as i8 as f32 / I8_SCALE).collect();
        let c = cosine(&v, &deq).unwrap();
        assert!(
            (c - 1.0).abs() < 0.02,
            "quantization error is tiny, got {c}"
        );
    }

    #[test]
    fn rrf_prefers_ids_ranked_well_in_both_lists() {
        let fused = rrf_fuse(&[10, 20, 30], &[20, 40, 10], 10);
        // 20 and 10 appear in both lists → top; 20 ranks higher overall.
        assert_eq!(&fused[..2], &[20, 10]);
        assert!(fused.contains(&30) && fused.contains(&40));
        assert_eq!(fused.len(), 4);
    }

    #[test]
    fn rrf_is_deterministic_on_ties_and_respects_limit() {
        // Same ranks in both lists: ties break by ascending id.
        assert_eq!(rrf_fuse(&[7, 3], &[7, 3], 10), vec![7, 3]);
        assert_eq!(rrf_fuse(&[1, 2, 3], &[], 2), vec![1, 2]);
        assert!(rrf_fuse(&[], &[], 5).is_empty());
    }
}
