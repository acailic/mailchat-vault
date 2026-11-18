//! RFC 6962-style Merkle tree over per-message digests, plus inclusion
//! proofs — the P3·20 export-verification base (Merkle 1980; the
//! domain-separated hashing and proof shape follow RFC 6962, as used by
//! Certificate Transparency and Trillian).
//!
//! # Hashing rules
//!
//! - A **leaf** node is `SHA-256(0x00 || leaf-digest)`, where the leaf
//!   digest is the per-message digest the export records (see
//!   [`crate::export_html`]'s `message_digest`).
//! - An **internal** node is `SHA-256(0x01 || left || right)`.
//!   The `0x00`/`0x01` domain separators make leaf and node hashes
//!   indistinguishable to a second-preimage attack.
//! - **Non-power-of-two sizes:** each level pairs adjacent nodes
//!   (`0+1`, `2+3`, …) and a lone trailing node is *promoted unchanged*
//!   to the next level (no duplication) — the rule this module and the
//!   bundle manifest both document.
//!
//! # Ordering rule
//!
//! The caller fixes the leaf order; the export bundle uses messages
//! sorted by `external_id` (byte order), ties broken by digest, so the
//! same data always yields the same root. This module itself treats
//! leaves as an ordered slice.
//!
//! A single-leaf tree's root is that leaf's hash with an empty proof
//! path; an empty leaf set has no root ([`merkle_root`] returns
//! `None`).

use sha2::{Digest as _, Sha256};

/// SHA-256 output size in bytes.
pub const DIGEST_BYTES: usize = 32;

/// A SHA-256 digest (message digest, node hash, or root).
pub type Digest = [u8; DIGEST_BYTES];

/// Which side of the pair the proof's sibling hash sits on: `Left`
/// means the sibling is the left operand, `Right` the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// One inclusion-proof step: the sibling hash and its side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProofStep {
    pub side: Side,
    pub sibling: Digest,
}

/// The leaf hash of one digest: `SHA-256(0x00 || digest)`.
pub fn leaf_hash(digest: &Digest) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update([0x00]);
    hasher.update(digest);
    hasher.finalize().into()
}

/// The internal node hash: `SHA-256(0x01 || left || right)`.
fn node_hash(left: &Digest, right: &Digest) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// The root over `leaves` in the given order, or `None` when there are
/// no leaves. Pair adjacent nodes per level; promote a lone trailing
/// node unchanged.
pub fn merkle_root(leaves: &[Digest]) -> Option<Digest> {
    let mut level: Vec<Digest> = leaves.iter().map(leaf_hash).collect();
    if level.is_empty() {
        return None;
    }
    while level.len() > 1 {
        level = next_level(&level);
    }
    Some(level[0])
}

/// One level up: pair adjacent hashes, promote a lone trailing node.
fn next_level(level: &[Digest]) -> Vec<Digest> {
    let mut up = Vec::with_capacity(level.len().div_ceil(2));
    let mut i = 0;
    while i + 1 < level.len() {
        up.push(node_hash(&level[i], &level[i + 1]));
        i += 2;
    }
    if i < level.len() {
        up.push(level[i]);
    }
    up
}

/// The inclusion-proof path for every leaf: step `i` of a path is the
/// sibling hash the leaf's node combines with at tree level `i`, and
/// which side that sibling sits on. A single-leaf tree yields one empty
/// path; an empty leaf set yields no paths.
pub fn inclusion_proofs(leaves: &[Digest]) -> Vec<Vec<ProofStep>> {
    let mut proofs: Vec<Vec<ProofStep>> = vec![Vec::new(); leaves.len()];
    if leaves.is_empty() {
        return proofs;
    }
    let mut level: Vec<Digest> = leaves.iter().map(leaf_hash).collect();
    // Each leaf's node index at the current level.
    let mut index: Vec<usize> = (0..leaves.len()).collect();
    while level.len() > 1 {
        for i in 0..leaves.len() {
            let idx = index[i];
            if idx % 2 == 1 {
                // Left sibling.
                proofs[i].push(ProofStep {
                    side: Side::Left,
                    sibling: level[idx - 1],
                });
            } else if idx + 1 < level.len() {
                // Right sibling.
                proofs[i].push(ProofStep {
                    side: Side::Right,
                    sibling: level[idx + 1],
                });
            }
            // A lone trailing even index is promoted unchanged: no step.
            index[i] = idx / 2;
        }
        level = next_level(&level);
    }
    proofs
}

/// The root one leaf digest plus its proof path proves membership in —
/// folding each step's sibling hash in from its side. This is the whole
/// verification: compare the result against the expected root.
pub fn prove(digest: &Digest, path: &[ProofStep]) -> Digest {
    let mut current = leaf_hash(digest);
    for step in path {
        current = match step.side {
            Side::Left => node_hash(&step.sibling, &current),
            Side::Right => node_hash(&current, &step.sibling),
        };
    }
    current
}

/// Full-set verification: whether `leaves`, in the given order, hash to
/// `root`. An empty leaf set matches no root.
pub fn verify_root(leaves: &[Digest], root: &Digest) -> bool {
    match merkle_root(leaves) {
        Some(computed) => &computed == root,
        None => false,
    }
}

/// Lowercase hex encoding of a digest (the manifest's wire form).
pub fn hex(digest: &Digest) -> String {
    let mut text = String::with_capacity(DIGEST_BYTES * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Parse 64 lowercase-or-uppercase hex chars into a digest.
pub fn unhex(text: &str) -> Option<Digest> {
    let bytes = text.as_bytes();
    if bytes.len() != DIGEST_BYTES * 2 {
        return None;
    }
    let mut out = [0u8; DIGEST_BYTES];
    for (i, chunk) in bytes.chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digests(n: usize) -> Vec<Digest> {
        (0..n)
            .map(|i| {
                let mut d = [0u8; DIGEST_BYTES];
                d[..8].copy_from_slice(&(i as u64).to_be_bytes());
                d
            })
            .collect()
    }

    #[test]
    fn empty_set_has_no_root_and_no_paths() {
        assert_eq!(merkle_root(&[]), None);
        assert!(inclusion_proofs(&[]).is_empty());
    }

    #[test]
    fn single_leaf_root_is_its_leaf_hash_with_an_empty_path() {
        let leaves = digests(1);
        assert_eq!(merkle_root(&leaves), Some(leaf_hash(&leaves[0])));
        let proofs = inclusion_proofs(&leaves);
        assert_eq!(proofs.len(), 1);
        assert!(proofs[0].is_empty());
        assert_eq!(prove(&leaves[0], &proofs[0]), leaf_hash(&leaves[0]));
    }

    #[test]
    fn every_proof_proves_the_computed_root_for_any_size() {
        for n in [2, 3, 4, 5, 6, 7, 8, 9, 12, 16, 17] {
            let leaves = digests(n);
            let root = merkle_root(&leaves).unwrap();
            assert!(verify_root(&leaves, &root), "n={n}: set verifies");
            for (i, path) in inclusion_proofs(&leaves).iter().enumerate() {
                assert_eq!(
                    prove(&leaves[i], path),
                    root,
                    "n={n}: leaf {i} proves the root"
                );
            }
        }
    }

    #[test]
    fn order_changes_the_root_and_mismatches_fail_verification() {
        let leaves = digests(3);
        let root = merkle_root(&leaves).unwrap();
        let mut swapped = leaves.clone();
        swapped.swap(0, 1);
        assert_ne!(merkle_root(&swapped).unwrap(), root);
        assert!(!verify_root(&swapped, &root));
        let mut flipped = leaves.clone();
        flipped[0][0] ^= 0xFF;
        assert!(!verify_root(&flipped, &root));
    }

    #[test]
    fn proof_size_grows_logarithmically() {
        let cases = [
            (2usize, 1usize),
            (4, 2),
            (8, 3),
            (16, 4),
            (5, 3), // promotion adds one level for 5 leaves
        ];
        for (n, max_len) in cases {
            let paths = inclusion_proofs(&digests(n));
            assert!(
                paths.iter().all(|p| p.len() <= max_len),
                "n={n}: every path holds at most {max_len} hashes"
            );
        }
    }

    #[test]
    fn hex_round_trips() {
        let d = digests(1)[0];
        let text = hex(&d);
        assert_eq!(text.len(), 64);
        assert_eq!(unhex(&text), Some(d));
        assert_eq!(unhex(&text.to_uppercase()), Some(d));
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex(&text[..63]), None);
    }
}
