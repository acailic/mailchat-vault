//! Small shared helpers for the ingest sources.

/// FNV-1a offset basis and prime step.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a_step(mut acc: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        acc ^= u64::from(b);
        acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
    }
    acc
}

/// FNV-1a content hash: tiny, deterministic, and stable across runs. Used
/// for messages whose export carries no stable id, so re-importing the
/// same content always dedups.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_step(FNV_OFFSET, bytes)
}

/// FNV-1a over several string fields, each hashed length-prefixed so two
/// different field splittings can never collide. Chat exports have no
/// message ids, so the stable tuple (chat, author, timestamp, body) — plus
/// an occurrence counter for identical repeats, kept by the caller —
/// becomes the dedup key.
pub(crate) fn fnv1a_fields(parts: &[&str]) -> u64 {
    let mut acc = FNV_OFFSET;
    for part in parts {
        acc = fnv1a_step(acc, &(part.len() as u64).to_le_bytes());
        acc = fnv1a_step(acc, part.as_bytes());
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_deterministic_and_input_sensitive() {
        assert_eq!(fnv1a(b"hello"), fnv1a(b"hello"));
        assert_ne!(fnv1a(b"hello"), fnv1a(b"hellp"));
    }

    #[test]
    fn fnv1a_fields_separates_different_splits() {
        // Without length prefixes ("ab", "c") and ("a", "bc") would hash
        // to the same bytes.
        assert_ne!(fnv1a_fields(&["ab", "c"]), fnv1a_fields(&["a", "bc"]));
        assert_eq!(fnv1a_fields(&["a", "b"]), fnv1a_fields(&["a", "b"]));
    }
}
