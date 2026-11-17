//! SimHash near-duplicate fingerprints (P2.12, storage slice).
//!
//! Scientific basis: G. Manku, A. Jain & A. Das Sarma,
//! "Detecting Near-Duplicates for Web Crawling", WWW 2007
//! (doi:10.1145/1242572.1242592), with lineage in A. Broder's
//! shingling/resemblance work ("On the Resemblance and Containment of
//! Documents", 1997). SimHash maps a document to a 64-bit fingerprint
//! with the property that edit-similar documents differ in few bits:
//! each bit is the majority vote of the shingle hashes' corresponding
//! bits, so adding or removing a small fraction of shingles flips only
//! a small number of majority outcomes. Web-scale practice treats
//! Hamming distance <= 3 as "near-duplicate"; the query-time folding
//! that consumes these fingerprints (follow-up slices) uses the same
//! threshold.
//!
//! This module is pure and dependency-free by design: the fingerprint
//! of a given text is a stable function of that text (no std hasher,
//! whose layout is allowed to change between Rust releases). Shingles
//! are k=4 word windows over Unicode word boundaries, lowercased, so
//! Latin and non-Latin (e.g. Cyrillic) text segment identically every
//! run. Storage and the import-time write live in [`crate::storage`].

/// Number of consecutive words in one shingle (k in the shingling
/// literature; 4 keeps shingles specific enough for mail/chat bodies
/// while surviving line wrapping and quote markers).
const SHINGLE_WIDTH: usize = 4;

/// FNV-1a 64-bit, the deterministic stand-in for a hash function this
/// module cannot take from std (`DefaultHasher` makes no stability
/// promise). Only used to weigh shingles, never persisted on its own.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Lowercased words between non-word boundaries, Unicode-aware: a word
/// is a maximal run of alphanumeric characters (`char::is_alphanumeric`
/// carries the Unicode letter/number classes), so Cyrillic or CJK text
/// segments by the same rule as Latin. Punctuation, whitespace, and
/// quote markers (">") act as separators — quoted-reply bodies shingle
/// close to their originals.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// 64-bit SimHash fingerprint of `text`.
///
/// The text is segmented into lowercased Unicode words, hashed as k=4
/// word shingles (a document with fewer than k words contributes its
/// whole word list as one shingle, so short chat lines still hash
/// stably), the shingles are deduplicated — Broder's resemblance is a
/// set similarity, so a full quoted copy of the body adds no new
/// shingles — and each fingerprint bit is the sign of the summed
/// per-bit votes of the shingle hashes (Manku et al. 2007, fig. 1).
pub fn simhash64(text: &str) -> u64 {
    let words = words(text);
    if words.is_empty() {
        return 0;
    }
    let mut shingles: Vec<u64> = Vec::new();
    if words.len() <= SHINGLE_WIDTH {
        shingles.push(shingle_hash(&words));
    } else {
        for window in words.windows(SHINGLE_WIDTH) {
            shingles.push(shingle_hash(window));
        }
    }
    shingles.sort_unstable();
    shingles.dedup();

    let mut votes = [0i64; 64];
    for hash in shingles {
        for (bit, vote) in votes.iter_mut().enumerate() {
            if (hash >> bit) & 1 == 1 {
                *vote += 1;
            } else {
                *vote -= 1;
            }
        }
    }
    let mut fingerprint = 0u64;
    for (bit, &vote) in votes.iter().enumerate() {
        if vote > 0 {
            fingerprint |= 1 << bit;
        }
    }
    fingerprint
}

/// Hash one shingle: the FNV-1a of its lowercased words joined by a
/// space separator (the separator keeps "ab c" from colliding with
/// "a bc").
fn shingle_hash(window: &[String]) -> u64 {
    let mut joined = String::new();
    for (i, word) in window.iter().enumerate() {
        if i > 0 {
            joined.push(' ');
        }
        joined.push_str(word);
    }
    fnv1a64(joined.as_bytes())
}

/// Hamming distance between two fingerprints: the number of differing
/// bits. Query-time near-duplicate folding (follow-up slice) treats
/// distance <= 3 as a match, per Manku et al.'s web-scale finding.
pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_hashes_identically() {
        assert_eq!(
            simhash64("meeting moved to thursday please update the calendar invite"),
            simhash64("meeting moved to thursday please update the calendar invite")
        );
    }

    #[test]
    fn short_text_is_stable_and_nonzero() {
        // Fewer than k words: the whole word list is one shingle.
        assert_eq!(simhash64("ok"), simhash64("ok"));
        assert_ne!(simhash64("ok"), simhash64("cancel"));
        assert_eq!(simhash64(""), 0);
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    }
}
