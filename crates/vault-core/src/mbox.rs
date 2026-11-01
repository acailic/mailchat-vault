//! `.mbox` file source: a manual/demo import that keeps the ingest line
//! executable without any IMAP access.
//!
//! Supports the common `mboxo`/`mboxrd` shape: messages separated by a line
//! starting with `From ` (sender + date), with bodies escaping embedded
//! `From ` lines as `>From ` (the escaped form does not split a message).

use std::fs;
use std::path::PathBuf;

use crate::ingest::{Collected, Source};
use crate::mail::parse_raw_mail;
use crate::Result;

/// Source name and dedup namespace for mbox imports.
pub const MBOX_SOURCE: &str = "mbox";

/// Reads one `.mbox` file and yields its messages.
#[derive(Debug, Clone)]
pub struct MboxSource {
    path: PathBuf,
}

impl MboxSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for MboxSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        let chat_id = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_mbox(&bytes, &chat_id)
    }
}

/// Parse raw `.mbox` file contents into messages.
///
/// `chat_id` groups the messages (typically the file name stem). Message
/// parsing itself lives in [`crate::mail`] and is shared with the IMAP
/// source. Mails that fail to parse are counted in `Collected::failed`
/// rather than silently dropped.
pub fn parse_mbox(bytes: &[u8], chat_id: &str) -> Result<Collected> {
    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for raw in split_mbox(bytes) {
        // Drop the leading "From sender date" separator line before parsing.
        let raw = match raw.iter().position(|&b| b == b'\n') {
            Some(nl) => &raw[nl + 1..],
            None => continue, // separator line with no content
        };
        if raw.is_empty() {
            continue;
        }
        match parse_raw_mail(raw, MBOX_SOURCE, chat_id) {
            Some(msg) => messages.push(msg),
            None => failed += 1,
        }
    }
    Ok(Collected { messages, failed })
}

/// Split on lines starting with `From `, except escaped `>From ` lines.
fn split_mbox(bytes: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i < bytes.len() {
        // Find the next line start and check what the line begins with.
        if starts_with_from_line(&bytes[i..]) && (i == 0 || bytes[i - 1] == b'\n') {
            if let Some(prev) = start.replace(i) {
                parts.push(&bytes[prev..i]);
            }
        }
        // Advance to the next line.
        match bytes[i..].iter().position(|&b| b == b'\n') {
            Some(nl) => i += nl + 1,
            None => break,
        }
    }
    if let Some(prev) = start {
        parts.push(&bytes[prev..]);
    }
    parts
}

fn starts_with_from_line(line: &[u8]) -> bool {
    line.starts_with(b"From ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `depth`-deep nested multipart mail, properly closed.
    fn nested_multipart(depth: usize) -> Vec<u8> {
        let mut s = String::new();
        for d in 0..depth {
            s.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"b{d}\"\n\n"
            ));
            s.push_str(&format!("--b{d}\n"));
        }
        s.push_str("Content-Type: text/plain\n\nhi\n");
        for d in (0..depth).rev() {
            s.push_str(&format!("--b{d}--\n"));
        }
        s.into_bytes()
    }

    #[test]
    fn hostile_deep_multipart_is_failed_not_a_crash() {
        // Regression: before the depth guard this overflowed the stack in
        // mailparse's recursive multipart descent (SIGABRT, not an error).
        for depth in [2000usize, 20_000, 100_000] {
            let mut mail = nested_multipart(depth);
            let bytes = [b"From a@b.c\n".as_slice(), &mail].concat();
            mail.clear();
            let collected = parse_mbox(&bytes, "f").unwrap();
            assert_eq!(collected.messages.len(), 0, "depth {depth}");
            assert_eq!(collected.failed, 1, "depth {depth}");
        }
    }

    #[test]
    fn moderate_nesting_still_parses() {
        let mail = nested_multipart(8);
        let bytes = [b"From a@b.c\n".as_slice(), &mail].concat();
        let collected = parse_mbox(&bytes, "f").unwrap();
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.messages[0].body, "hi");
    }
}
