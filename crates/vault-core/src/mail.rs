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

/// Parse one raw RFC 5322 message into a [`NewMessage`].
///
/// `external_id` is the `Message-ID` header (angle brackets trimmed),
/// falling back to a deterministic content hash when the header is absent —
/// so re-collecting the same message never stores a second row. `body` is
/// the decoded text body prefixed with the subject so both are full-text
/// searchable; `raw_json` keeps the key headers plus `chat_id`-independent
/// metadata. Returns `None` when the mail fails to parse at all.
pub(crate) fn parse_raw_mail(raw: &[u8], source: &str, chat_id: &str) -> Option<NewMessage> {
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
