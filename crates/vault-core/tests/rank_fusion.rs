//! Unit tests for the pure Reciprocal Rank Fusion module
//! ([`vault_core::rank_fusion`]): the fuse step that merges the FTS leg
//! and the vector leg (and, in the general form, any N ranked id lists)
//! of the hybrid Ask pipeline.
//!
//! The behavior contract pinned here: empty inputs stay empty, ids found
//! by more lists (or at better ranks) outrank the rest, equal scores
//! tie-break by ascending id, output is reproducible across runs, and the
//! limit truncates. The ranking-quality evidence (recall@10 / nDCG@10
//! floors, fused-vs-FTS-only guard) lives in `tests/retrieval_eval.rs`.

use vault_core::rank_fusion::{rrf_fuse, rrf_fuse_n, RRF_K};

#[test]
fn empty_lists_fuse_to_empty() {
    assert!(rrf_fuse(&[], &[], 10).is_empty());
    assert!(rrf_fuse_n(&[], 10).is_empty());
    assert!(rrf_fuse_n(&[&[], &[], &[]], 10).is_empty());
    // One empty leg must not disturb the other leg's order.
    assert_eq!(rrf_fuse(&[4, 2, 9], &[], 10), vec![4, 2, 9]);
    assert_eq!(rrf_fuse(&[], &[7, 1], 10), vec![7, 1]);
    // A zero limit truncates everything, even with input.
    assert!(rrf_fuse(&[1, 2], &[3], 0).is_empty());
}

#[test]
fn disjoint_lists_rank_by_reciprocal_rank_alone() {
    // No id overlaps: each id's score is a single 1/(k + rank) term, so
    // better ranks in either list win and same-rank ids tie by id.
    let fused = rrf_fuse(&[30, 40], &[10, 20], 10);
    // Rank 1: 30 and 10 (tie -> 10 first); rank 2: 40 and 20 (tie -> 20 first).
    assert_eq!(fused, vec![10, 30, 20, 40]);
}

#[test]
fn ids_ranked_well_in_both_lists_outrank_single_list_hits() {
    let fused = rrf_fuse(&[10, 20, 30], &[20, 40, 10], 10);
    // 20 (ranks 2+1) edges 10 (ranks 1+3) — both beat the single-list hits.
    assert_eq!(&fused[..2], &[20, 10]);
    assert!(fused.contains(&30) && fused.contains(&40));
    assert_eq!(fused.len(), 4);
}

#[test]
fn scores_follow_the_k60_formula() {
    // k = 60 is the published constant (Cormack, Buettner & Clarke,
    // SIGIR 2009, doi:10.1145/1571941.1572114).
    assert_eq!(RRF_K, 60);
    let inv = |rank: usize| 1.0f32 / (RRF_K + rank as u32) as f32; // 1-based rank
    let fused = rrf_fuse(&[100, 200], &[300, 400, 500, 200], 10);
    // 200 fuses inv(2) + inv(4) ≈ 0.0318, beating the best single-list hit
    // inv(1) ≈ 0.0164 — an order that flips under k = 0, so this pins k.
    assert!(inv(2) + inv(4) > inv(1));
    assert_eq!(fused[0], 200);
}

#[test]
fn equal_scores_tie_break_by_ascending_id() {
    // Identical ranks in both lists: exact score ties, ascending id.
    assert_eq!(rrf_fuse(&[1, 2], &[2, 1], 10), vec![1, 2]);
    // Every id at rank 1 of its own list: the whole output is ascending.
    assert_eq!(rrf_fuse_n(&[&[9], &[4], &[1], &[7]], 10), vec![1, 4, 7, 9]);
}

#[test]
fn n_list_fusion_prefers_ids_found_by_more_lists() {
    // Three lists; 2 appears in two lists at good ranks, 1 and 4 also
    // appear twice but 1 vs 4 is an exact tie (rank 1 + rank 3 each),
    // single-list hits 5/6 tie below them, and 3 trails at rank 3.
    let fused = rrf_fuse_n(&[&[1, 2, 3], &[4, 5, 1], &[2, 6, 4]], 10);
    assert_eq!(fused, vec![2, 1, 4, 5, 6, 3]);
}

#[test]
fn fusion_is_deterministic_across_repeated_calls() {
    // Many ids across three permuted lists (ties included): HashMap
    // iteration order varies per call, but the score-then-id sort is
    // total, so every call must return the identical ordering.
    let a: Vec<i64> = (0..40).map(|i| (i * 37) % 40).collect();
    let b: Vec<i64> = (0..40).map(|i| (i * 41 + 13) % 40).collect();
    let c: Vec<i64> = (0..40).rev().map(|i| i as i64).collect();
    let first = rrf_fuse_n(&[&a, &b, &c], 25);
    assert_eq!(first.len(), 25);
    for _ in 0..50 {
        assert_eq!(rrf_fuse_n(&[&a, &b, &c], 25), first);
    }
}

#[test]
fn limit_truncates_after_sorting() {
    // 3 tops both lists (1/61 + 1/61); 1 and 2 tie for second
    // (1/62 + 1/63 each), so the top-2 window is exactly [3, 1].
    assert_eq!(rrf_fuse(&[3, 1, 2], &[3, 2, 1], 2), vec![3, 1]);
}

#[test]
fn two_list_entry_point_matches_the_n_list_form() {
    let fts = [5, 6, 7, 8];
    let vec = [7, 9, 5, 1];
    assert_eq!(rrf_fuse(&fts, &vec, 10), rrf_fuse_n(&[&fts, &vec], 10));
    assert_eq!(rrf_fuse(&fts, &vec, 3), rrf_fuse_n(&[&fts, &vec], 3));
}
