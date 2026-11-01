//! Shared RFC 5322 mail parsing: raw message bytes → [`NewMessage`].
//!
//! Both ingest sources that deal with raw mail bytes use this module: the
//! `.mbox` file source ([`crate::mbox`]) and the IMAP source
//! ([`crate::imap`]).

use chrono::{TimeZone, Utc};
use mailparse::{parse_mail, MailHeaderMap, ParsedMail};
use serde_json::json;

use crate::message::NewMessage;
use crate::util::fnv1a;

/// Maximum MIME multipart nesting accepted from one mail.
///
/// `mailparse` descends multipart structure recursively, so a hostile
/// mail nesting thousands of multiparts would overflow the stack and
/// abort the whole process — the one way a malformed message could crash
/// an ingest source instead of being reported as a parse failure. Real
/// mails nest three to five levels at most, so anything beyond this bound
/// is treated as malformed: [`parse_raw_mail`] returns `None` and callers
/// count the message as failed.
const MAX_MIME_DEPTH: usize = 64;

/// Parse one raw RFC 5322 message into a [`NewMessage`].
///
/// `external_id` is the `Message-ID` header (angle brackets trimmed),
/// falling back to a deterministic content hash when the header is absent —
/// so re-collecting the same message never stores a second row. `body` is
/// the decoded text body prefixed with the subject so both are full-text
/// searchable; `raw_json` keeps the key headers plus `chat_id`-independent
/// metadata. Returns `None` when the mail fails to parse at all.
pub(crate) fn parse_raw_mail(raw: &[u8], source: &str, chat_id: &str) -> Option<NewMessage> {
    if mime_nesting_depth(raw) > MAX_MIME_DEPTH {
        return None;
    }
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

    let mut msg = NewMessage::new(source, message_id, author, sent_at, body);
    msg.chat_id = chat_id.to_string();
    msg.raw_json = raw_json;
    Some(msg)
}

/// Amend a parsed message's `raw_json` extras with additional keys.
///
/// Sources that carry extra per-message provenance (e.g. IMAP folder and
/// UID) splice those keys into the JSON object produced by
/// [`parse_raw_mail`]. Parse failures of the (always self-produced) JSON
/// leave the message unchanged.
pub(crate) fn amend_raw_json(msg: &mut NewMessage, extras: &[(&str, serde_json::Value)]) {
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&msg.raw_json) {
        if let Some(obj) = value.as_object_mut() {
            for (key, v) in extras {
                obj.insert((*key).to_string(), v.clone());
            }
            msg.raw_json = value.to_string();
        }
    }
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

/// Conservative overestimate of a mail's MIME multipart nesting depth: the
/// deepest stack of simultaneously open `multipart` boundaries.
///
/// `mailparse` recurses once per nesting level it descends into, and it
/// descends exactly when a part's (folded) `Content-Type` header declares a
/// `multipart/…` type with a `boundary` parameter, closing the level at the
/// matching `--boundary--` line — the same events this line scan counts, so
/// it never counts lower than the recursion `mailparse` would perform. On
/// malformed structure it counts high (a boundary that never closes stays
/// open), which only rejects junk earlier.
fn mime_nesting_depth(raw: &[u8]) -> usize {
    let mut open: Vec<Vec<u8>> = Vec::new();
    let mut depth = 0;
    for line in UnfoldedLines::new(raw) {
        if let Some(boundary) = multipart_boundary(&line) {
            open.push(boundary);
            depth = depth.max(open.len());
        } else if let Some(top) = open.last() {
            let mut close = Vec::with_capacity(top.len() + 4);
            close.extend_from_slice(b"--");
            close.extend_from_slice(top);
            close.extend_from_slice(b"--");
            if line.starts_with(&close) {
                open.pop();
            }
        }
    }
    depth
}

/// Iterate a mail's logical lines: physical lines split on `\n` with the
/// trailing `\r` dropped, and continuation lines (starting with a space or
/// tab) folded into the previous line, the way `mailparse` unfolds header
/// values before reading parameters such as `boundary=`.
struct UnfoldedLines<'a> {
    raw: &'a [u8],
    pos: usize,
}

impl<'a> UnfoldedLines<'a> {
    fn new(raw: &'a [u8]) -> Self {
        Self { raw, pos: 0 }
    }
}

impl<'a> Iterator for UnfoldedLines<'a> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        if self.pos >= self.raw.len() {
            return None;
        }
        let mut logical: Vec<u8> = Vec::new();
        loop {
            let start = self.pos;
            let end = self.raw[start..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(self.raw.len(), |nl| start + nl);
            let mut line = &self.raw[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            self.pos = (end + 1).min(self.raw.len());
            let starts_new_logical =
                !logical.is_empty() && !line.first().is_some_and(|&b| b == b' ' || b == b'\t');
            if starts_new_logical {
                // A new logical line starts here; leave it for the next
                // call.
                self.pos = start;
                break;
            }
            logical.extend_from_slice(line);
            if self.pos >= self.raw.len() {
                break;
            }
        }
        Some(logical)
    }
}

/// The `boundary` parameter of a `Content-Type: multipart/…` header line,
/// quoted or bare — mirroring how `mailparse` reads it. `None` when the
/// line is not a multipart content-type declaring a boundary parameter.
fn multipart_boundary(line: &[u8]) -> Option<Vec<u8>> {
    let lowered: Vec<u8> = line.iter().map(|b| b.to_ascii_lowercase()).collect();
    if !lowered.starts_with(b"content-type:") || !contains(&lowered, b"multipart/") {
        return None;
    }
    let at = find(&lowered, b"boundary=")?;
    // The value runs to the next ';' (parameter separator) or line end,
    // trimmed, with optional quotes stripped — as mailparse parses it.
    let value = &line[at + b"boundary=".len()..];
    let end = value.iter().position(|&b| b == b';').unwrap_or(value.len());
    let value = value[..end].trim_ascii();
    let value = if value.len() >= 2 && value.first() == Some(&b'"') && value.last() == Some(&b'"') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    Some(value.to_vec())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
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
    fn nesting_depth_counts_open_boundaries() {
        let plain = b"From a@b.c\nSubject: s\n\nbody\n";
        assert_eq!(mime_nesting_depth(plain), 0);
        assert_eq!(mime_nesting_depth(&nested_multipart(1)), 1);
        assert_eq!(mime_nesting_depth(&nested_multipart(3)), 3);
        // A boundary that never closes stays counted.
        let unclosed =
            b"Content-Type: multipart/mixed; boundary=\"x\"\n\n--x\nContent-Type: text/plain\n\nbody\n";
        assert_eq!(mime_nesting_depth(unclosed), 1);
    }

    #[test]
    fn nesting_depth_unfolds_continuation_lines() {
        let folded = b"Content-Type: multipart/mixed;\n boundary=\"bb\"\n\n--bb\n\nbody\n--bb--\n";
        assert_eq!(mime_nesting_depth(folded), 1);
    }

    #[test]
    fn nesting_depth_ignores_non_multipart_content_types() {
        for line in [
            &b"Content-Type: text/plain; boundary=\"x\""[..],
            b"X-Content-Type: multipart/mixed; boundary=\"x\"",
        ] {
            assert_eq!(multipart_boundary(line), None, "{line:?}");
        }
        assert_eq!(
            multipart_boundary(b"CONTENT-TYPE: MULTIPART/MIXED; BOUNDARY=\"Q\""),
            Some(b"Q".to_vec())
        );
    }

    #[test]
    fn hostile_deep_multipart_is_rejected_not_a_crash() {
        // Regression: before the depth guard this overflowed the stack in
        // mailparse's recursive multipart descent (SIGABRT, not an error).
        for depth in [2000usize, 20_000, 100_000] {
            let mut mail = nested_multipart(depth);
            assert!(
                parse_raw_mail(&mail, "mbox", "f").is_none(),
                "depth {depth}"
            );
            mail.clear();
        }
    }

    #[test]
    fn moderate_nesting_still_parses() {
        let msg = parse_raw_mail(&nested_multipart(8), "mbox", "f").unwrap();
        assert_eq!(msg.body, "hi");
    }
}
