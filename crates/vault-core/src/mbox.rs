//! `.mbox` file source: a manual/demo import that keeps the ingest line
//! executable without any IMAP access.
//!
//! Supports the common `mboxo`/`mboxrd` shape: messages separated by a line
//! starting with `From ` (sender + date), with bodies escaping embedded
//! `From ` lines as `>From ` (the escaped form does not split a message).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use mailparse::{parse_mail, MailHeaderMap};
use serde_json::json;

use crate::ingest::Source;
use crate::message::NewMessage;
use crate::{Error, Result};

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

    fn collect(&self) -> Result<Vec<NewMessage>> {
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
/// `chat_id` groups the messages (typically the file name stem). For each
/// mail: `external_id` is the `Message-ID` (falling back to a content hash
/// when absent), `body` is the decoded text body prefixed with the subject so
/// both are full-text searchable, and `raw_json` keeps the key headers.
pub fn parse_mbox(bytes: &[u8], chat_id: &str) -> Result<Vec<NewMessage>> {
    let mut messages = Vec::new();
    for raw in split_mbox(bytes) {
        // Drop the leading "From sender date" separator line before parsing.
        let raw = match raw.iter().position(|&b| b == b'\n') {
            Some(nl) => &raw[nl + 1..],
            None => continue, // separator line with no content
        };
        if raw.is_empty() {
            continue;
        }
        if let Some(msg) = parse_single(raw, chat_id) {
            messages.push(msg);
        }
    }
    Ok(messages)
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

fn parse_single(raw: &[u8], chat_id: &str) -> Option<NewMessage> {
    let mail = parse_mail(raw).ok()?;

    let message_id = mail
        .headers
        .get_first_value("Message-ID")
        .or_else(|| mail.headers.get_first_value("Message-Id"))
        .map(|v| v.trim().trim_matches(['<', '>']).to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("generated-{:016x}", fnv1a(raw)));

    let author = mail
        .headers
        .get_first_value("From")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();

    let sent_at = mail
        .headers
        .get_first_value("Date")
        .and_then(|v| mailparse::dateparse(&v).ok())
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();

    let subject = mail
        .headers
        .get_first_value("Subject")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();

    // Decoded text body; html-only mails (or decode failures) yield "".
    let text = mail
        .get_body()
        .map(|t| t.trim_end().to_string())
        .unwrap_or_default();
    let body = if subject.is_empty() {
        text
    } else if text.is_empty() {
        subject.clone()
    } else {
        format!("{subject}\n\n{text}")
    };

    let raw_json = json!({
        "message_id": message_id,
        "subject": subject,
        "from": author,
        "to": mail.headers.get_first_value("To").unwrap_or_default(),
        "date": mail.headers.get_first_value("Date").unwrap_or_default(),
    })
    .to_string();

    let mut msg = NewMessage::new(MBOX_SOURCE, message_id, author, sent_at, body);
    msg.chat_id = chat_id.to_string();
    msg.raw_json = raw_json;
    Some(msg)
}

/// FNV-1a: a tiny, fully deterministic content hash for mails without a
/// `Message-ID` (so re-importing the same file never duplicates them).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        acc ^= u64::from(b);
        acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
    }
    acc
}

/// Path helper for callers that validate user-provided paths.
pub fn validate_mbox_path(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(Error::MboxNotReadable(format!(
            "not a readable file: {}",
            path.display()
        )));
    }
    Ok(())
}
