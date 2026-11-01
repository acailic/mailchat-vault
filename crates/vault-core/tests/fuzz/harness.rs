//! Shared fuzz harness: run one case under `catch_unwind` and, on a
//! panic, fail with everything needed to reproduce the input.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run `body` for fuzz case `#idx` (`name` parser, `desc` provenance) and
/// turn a caught panic into a test failure that carries the reproduction
/// info: the case description (which includes the seed for random cases)
/// and a preview of the offending bytes.
///
/// Panics escaping here are the finding this suite exists for — never
/// catch, absorb, or skip them.
pub fn check_no_panic(name: &str, idx: usize, desc: &str, input: &[u8], body: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(body)) {
        let inner = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        panic!(
            "fuzz crash: {name} case #{idx} ({desc}) panicked: {inner}\n\
             input preview (latin-1 escaped): {}\n\
             reproduce: deterministic corpus case, or rerun its generator \
             with the seed recorded in the description above",
            preview(input),
        );
    }
}

/// Byte-accurate, escaped preview of the first 2 KiB of `input` (invalid
/// UTF-8 shows as escaped bytes, never lost).
pub fn preview(input: &[u8]) -> String {
    const MAX: usize = 2048;
    let shown: String = input.iter().take(MAX).map(|&b| b as char).collect();
    let tail = if input.len() > MAX {
        format!(" …+{} more bytes", input.len() - MAX)
    } else {
        String::new()
    };
    format!("{shown:?}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_escapes_and_truncates() {
        assert_eq!(preview(b"ab"), "\"ab\"");
        assert!(preview(&vec![0u8; 5000]).ends_with("more bytes"));
        assert!(preview(b"\x00").contains("\\0"));
    }

    #[test]
    fn passes_result_through() {
        let mut ran = false;
        check_no_panic("t", 0, "d", b"x", || ran = true);
        assert!(ran);
    }

    #[test]
    #[should_panic(expected = "fuzz crash")]
    fn converts_inner_panic_to_failure_with_repro() {
        check_no_panic("unit", 7, "seed=0x1", b"input", || {
            panic!("boom");
        });
    }
}
