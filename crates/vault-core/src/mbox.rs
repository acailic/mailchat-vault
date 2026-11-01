//! `.mbox` file source: a manual/demo import that keeps the ingest line
//! executable without any IMAP access.
//!
//! Supports the common `mboxo`/`mboxrd` shape: messages separated by a line
//! starting with `From ` (sender + date), with bodies escaping embedded
//! `From ` lines as `>From ` (the escaped form does not split a message).

use std::fs;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use mailparse::{parse_mail, MailHeaderMap, ParsedMail};
use serde_json::json;

use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a;
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
/// `chat_id` groups the messages (typically the file name stem). For each
/// mail: `external_id` is the `Message-ID` (falling back to a content hash
/// when absent), `body` is the decoded text body prefixed with the subject so
/// both are full-text searchable, and `raw_json` keeps the key headers.
///
/// Mails that fail to parse are counted in `Collected::failed` rather than
/// silently dropped.
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
        match parse_single(raw, chat_id) {
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

fn parse_single(raw: &[u8], chat_id: &str) -> Option<NewMessage> {
    let mail = parse_mail(raw).ok()?;

    let message_id = mail
        .headers
        .get_first_value("Message-ID")
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

    // Decoded text body: walk to the text/plain leaf parts, joined;
    // html-only or otherwise text-less mails fall back to the top-level
    // body, decode failures yield "".
    let text = extract_text(&mail)
        .or_else(|| decoded_body(&mail))
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

/// Collect the decoded text of every text/plain leaf part, joined with a
/// blank line; `None` when the mail has no text/plain leaf at all. Leaf
/// bodies go through `get_body`, so base64/quoted-printable transfer
/// encodings are decoded; a leaf whose decode fails contributes "".
fn extract_text(mail: &ParsedMail<'_>) -> Option<String> {
    if mail.subparts.is_empty() {
        return if mail.ctype.mimetype == "text/plain" {
            Some(decoded_body(mail).unwrap_or_default())
        } else {
            None
        };
    }
    let leaves: Vec<String> = mail.subparts.iter().filter_map(extract_text).collect();
    if leaves.is_empty() {
        None
    } else {
        Some(leaves.join("\n\n"))
    }
}

/// Decode a part's body to a String: `get_body_raw` handles the transfer
/// encoding; for bodies with no declared charset (or us-ascii) the bytes are
/// read as UTF-8 with a Latin-1 fallback for legacy 8-bit mail (mailparse's
/// own decode always maps them as Latin-1, garbling UTF-8 bodies); other
/// declared charsets go through mailparse's charset tables.
fn decoded_body(mail: &ParsedMail<'_>) -> Option<String> {
    let raw = mail.get_body_raw().ok()?;
    let charset = mail.ctype.charset.to_ascii_lowercase();
    if !charset.is_empty() && charset != "us-ascii" {
        return mail.get_body().ok().map(|t| t.trim_end().to_string());
    }
    Some(match String::from_utf8(raw) {
        Ok(s) => s.trim_end().to_string(),
        // Not UTF-8: map each byte as a Latin-1 code point (lossless).
        Err(e) => e.into_bytes().into_iter().map(|b| b as char).collect(),
    })
}
