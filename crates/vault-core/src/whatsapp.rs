//! WhatsApp chat export source: parses the `.txt` file produced by
//! WhatsApp's "Export chat" feature into searchable messages.
//!
//! Supported header shapes (system lines have no author part):
//! - `[1.11.2026., 09:00:05] Marko: Zdravo!` — bracketed; iOS and current
//!   Android exports.
//! - `1.11.2026., 09:00 - Marko: Zdravo!` — older Android exports.
//!
//! Dates are the exporter's local time and carry no UTC offset, so they
//! are stored verbatim in the RFC 3339 UTC form used across the archive.
//! Messages have no ids in the export; `external_id` is a content hash
//! (see [`parse_whatsapp`]), which keeps re-imports idempotent. Attachment
//! placeholders stay in the body with kind/name recorded in `raw_json`;
//! when the export ships with its files, the media pass (see
//! [`crate::media`]) matches the names and links the stored copies via a
//! `media` array in `raw_json`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::{json, Map, Value};

use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a_fields;
use crate::Result;

/// Source name and dedup namespace for WhatsApp imports.
pub const WHATSAPP_SOURCE: &str = "whatsapp";

/// Invisible bidi marks WhatsApp sprinkles through exported text; they
/// carry no searchable content and are stripped.
const BIDI_MARKS: [char; 2] = ['\u{200e}', '\u{200f}']; // LRM, RLM

/// Quoted-reply markers, per export language.
const QUOTED_MARKERS: [&str; 2] = ["[Quoted Message]", "[Citirana poruka]"];

/// Date formats where the day precedes the month (`dd/mm/yy` and friends).
/// Two-digit-year variants come first: chrono's `%Y` would happily parse
/// `26` as the year 26 AD, so the longer forms only match when the shorter
/// one leaves input behind.
const DATE_FORMATS_DAY_FIRST: &[&str] = &[
    "%d/%m/%y", "%d/%m/%Y", "%d-%m-%y", "%d-%m-%Y", "%d.%m.%y", "%d.%m.%Y", "%Y-%m-%d", "%Y/%m/%d",
];

/// Date formats where the month precedes the day (`mm/dd/yy` and friends).
const DATE_FORMATS_MONTH_FIRST: &[&str] = &[
    "%m/%d/%y", "%m/%d/%Y", "%m-%d-%y", "%m-%d-%Y", "%Y-%m-%d", "%Y/%m/%d",
];

/// Time formats: 24h and 12h (AM/PM), seconds optional. Input is
/// uppercased before parsing, so `%p` matches any letter case.
const TIME_FORMATS: &[&str] = &["%H:%M:%S", "%H:%M", "%I:%M:%S %p", "%I:%M %p"];

/// Reads one WhatsApp `.txt` export file and yields its messages.
#[derive(Debug, Clone)]
pub struct WhatsappSource {
    path: PathBuf,
}

impl WhatsappSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for WhatsappSource {
    fn name(&self) -> &str {
        WHATSAPP_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        let chat_id = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_whatsapp(&bytes, &chat_id)
    }
}

/// Parse WhatsApp export text into messages.
///
/// `chat_id` groups the messages (typically the export file's name stem,
/// e.g. `WhatsApp Chat with Ana`). Because the format has no message ids,
/// `external_id` is a content hash over chat id, author, timestamp, and
/// body, plus an occurrence counter so identical back-to-back messages
/// still get distinct rows — while re-importing the same file dedups
/// everything.
pub fn parse_whatsapp(bytes: &[u8], chat_id: &str) -> Result<Collected> {
    let text = decode_export(bytes);
    let day_first = detect_day_first(&text);

    let mut messages: Vec<NewMessage> = Vec::new();
    let mut occurrence: HashMap<u64, u32> = HashMap::new();
    let mut pending: Option<Pending> = None;

    for line in text.lines() {
        // Right after a quoted-reply marker the next line is the quoted
        // reference, even when it looks exactly like a message header
        // (WhatsApp echoes the full `[stamp] Author: text` line).
        let awaiting_quote = pending.as_ref().is_some_and(|p| p.awaiting_quote);
        if !awaiting_quote {
            if let Some(h) = parse_header(line, day_first) {
                finalize(pending.take(), chat_id, &mut messages, &mut occurrence);
                let awaiting_quote = is_quoted_marker(&strip_bidi(&h.body));
                pending = Some(Pending {
                    datetime: h.datetime,
                    author: h.author,
                    body: h.body,
                    awaiting_quote,
                });
                continue;
            }
        }
        if let Some(p) = pending.as_mut() {
            if !p.body.is_empty() {
                p.body.push('\n');
            }
            p.body.push_str(line);
            p.awaiting_quote = false;
        }
        // Lines before the first header (BOM leftovers, stray text) are
        // dropped.
    }
    finalize(pending, chat_id, &mut messages, &mut occurrence);

    Ok(Collected {
        messages,
        failed: 0,
    })
}

/// True when the text starts (within its first lines) with a parseable
/// WhatsApp header under either date ordering. Used to detect the format
/// of a picked "chat export" file.
pub fn is_whatsapp_export(bytes: &[u8]) -> bool {
    let text = decode_export(bytes);
    text.lines()
        .take(30)
        .any(|line| parse_header(line, true).is_some() || parse_header(line, false).is_some())
}

/// A message while its continuation lines are still being collected.
struct Pending {
    datetime: NaiveDateTime,
    author: Option<String>,
    body: String,
    /// Set while the next line must be consumed as the quoted reference.
    awaiting_quote: bool,
}

/// One parsed message header line.
struct Header {
    datetime: NaiveDateTime,
    /// `None` for system lines (e.g. the end-to-end-encryption banner).
    author: Option<String>,
    /// Text after the header, before continuation lines are appended.
    body: String,
}

/// Try to parse `line` as a message header under the given date order.
///
/// Bracketed stamps (`[1.11.2026., 09:00:05] Author: text`) are tried
/// first; a line starting with a digit is probed as the older Android
/// `1.11.2026., 09:00 - Author: text` shape, trying every ` - ` whose
/// left side parses as a stamp.
fn parse_header(line: &str, day_first: bool) -> Option<Header> {
    if let Some(rest) = line.strip_prefix('[') {
        let close = rest.find(']')?;
        let stamp = &rest[..close];
        let after = rest[close + 1..]
            .strip_prefix(' ')
            .unwrap_or(&rest[close + 1..]);
        let datetime = parse_stamp(stamp, day_first)?;
        let (author, body) = split_author(after);
        Some(Header {
            datetime,
            author,
            body,
        })
    } else if line.starts_with(|c: char| c.is_ascii_digit()) {
        let mut from = 0;
        while let Some(rel) = line[from..].find(" - ") {
            let at = from + rel;
            if let Some(datetime) = parse_stamp(&line[..at], day_first) {
                let (author, body) = split_author(&line[at + 3..]);
                return Some(Header {
                    datetime,
                    author,
                    body,
                });
            }
            from = at + 1;
        }
        None
    } else {
        None
    }
}

/// Split `Author: body` at the first `": "`. A rest with no `": "` at all
/// is a system line; a bare trailing `:` is an author with empty body.
fn split_author(rest: &str) -> (Option<String>, String) {
    match rest.find(": ") {
        Some(i) => (Some(rest[..i].to_string()), rest[i + 2..].to_string()),
        None if rest.ends_with(':') && !rest.is_empty() => {
            (Some(rest[..rest.len() - 1].to_string()), String::new())
        }
        None => (None, rest.to_string()),
    }
}

/// Parse a `date, time` stamp such as `1.11.2026., 09:00:05` (already
/// without brackets) or `6/28/23, 9:30:15 AM`.
fn parse_stamp(stamp: &str, day_first: bool) -> Option<NaiveDateTime> {
    let (date_part, time_part) = stamp.split_once(", ")?;
    let date_token = normalize_date_token(date_part);
    let time_token = normalize_time_token(time_part);
    if date_token.is_empty() || time_token.is_empty() {
        return None;
    }
    let date_formats = if day_first {
        DATE_FORMATS_DAY_FIRST
    } else {
        DATE_FORMATS_MONTH_FIRST
    };
    let date = date_formats
        .iter()
        .find_map(|f| NaiveDate::parse_from_str(&date_token, f).ok())?;
    let time = TIME_FORMATS
        .iter()
        .find_map(|f| NaiveTime::parse_from_str(&time_token, f).ok())?;
    Some(NaiveDateTime::new(date, time))
}

/// `"1. 11. 2026."` → `"1.11.2026"`: drop bidi marks and spaces, then the
/// trailing dot some locales print after the year.
fn normalize_date_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| !BIDI_MARKS.contains(c) && !c.is_whitespace())
        .collect::<String>()
        .trim_end_matches('.')
        .to_string()
}

/// Narrow/no-break spaces before AM/PM become plain spaces, bidi marks are
/// dropped, and the token is uppercased so `%p` matches any case.
fn normalize_time_token(token: &str) -> String {
    token
        .chars()
        .map(|c| match c {
            '\u{202f}' | '\u{00a0}' => ' ',
            c => c,
        })
        .filter(|c| !BIDI_MARKS.contains(c))
        .collect::<String>()
        .trim()
        .to_uppercase()
}

/// Decide `dd/mm` vs `mm/dd` ordering from the file's own dates: a first
/// component over 12 proves day-first (or is a year: `yyyy-mm-dd`), a
/// second component over 12 proves month-first. Ambiguous files default
/// to day-first, the common case outside the US.
fn detect_day_first(text: &str) -> bool {
    let mut day_first_seen = false;
    let mut month_first_seen = false;
    for line in text.lines().take(2000) {
        if let Some(token) = leading_date_token(line) {
            let comps: Vec<Option<u32>> = token
                .split(['/', '-', '.'])
                .map(|c| c.parse::<u32>().ok())
                .collect();
            if let (Some(Some(a)), Some(Some(b))) = (comps.first(), comps.get(1)) {
                if *a >= 1000 {
                    month_first_seen = true; // year-first ISO stamp
                } else if *a >= 13 {
                    day_first_seen = true;
                }
                if (13..1000).contains(b) {
                    month_first_seen = true;
                }
            }
        }
    }
    !(month_first_seen && !day_first_seen)
}

/// The normalized date token of a candidate header line, if it starts
/// like one (bracketed or hyphen-separated stamp).
fn leading_date_token(line: &str) -> Option<String> {
    let inner = if let Some(rest) = line.strip_prefix('[') {
        let close = rest.find(']')?;
        &rest[..close]
    } else if line.starts_with(|c: char| c.is_ascii_digit()) {
        let sep = line.find(" - ")?;
        &line[..sep]
    } else {
        return None;
    };
    let (date_part, _) = inner.split_once(", ")?;
    Some(normalize_date_token(date_part))
}

/// Decode export bytes as UTF-8 (lossy for stray invalid bytes) and drop a
/// leading BOM.
fn decode_export(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).into_owned();
    match text.strip_prefix('\u{feff}') {
        Some(stripped) => stripped.to_string(),
        None => text,
    }
}

fn strip_bidi(text: &str) -> String {
    text.chars().filter(|c| !BIDI_MARKS.contains(c)).collect()
}

fn is_quoted_marker(first_body_line: &str) -> bool {
    QUOTED_MARKERS.contains(&first_body_line)
}

/// Turn a finished [`Pending`] into a [`NewMessage`].
fn finalize(
    pending: Option<Pending>,
    chat_id: &str,
    messages: &mut Vec<NewMessage>,
    occurrence: &mut HashMap<u64, u32>,
) {
    let Some(p) = pending else { return };
    let author = p.author.map(|a| strip_bidi(&a)).unwrap_or_default();
    let sent_at = p.datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut body = strip_bidi(&p.body);
    let mut raw = Map::new();

    if is_quoted_marker(body.lines().next().unwrap_or("")) {
        if let Some((quoted_author, quoted_text, reply)) = split_quoted(&body) {
            raw.insert("quoted_author".into(), json!(quoted_author));
            raw.insert("quoted_text".into(), json!(quoted_text));
            body = reply;
        }
    }
    if let Some(attachment) = detect_attachment(&body) {
        raw.insert("attachment".into(), attachment);
    }
    if author.is_empty() {
        raw.insert("system".into(), json!(true));
    }

    // Content-hash dedup id; the occurrence counter keeps identical
    // back-to-back messages distinct while re-imports stay idempotent.
    let base = fnv1a_fields(&[chat_id, &author, &sent_at, &body]);
    let seen = occurrence.entry(base).or_insert(0);
    let external_id = format!("generated-{:016x}-{}", base, seen);
    *seen += 1;

    let mut msg = NewMessage::new(WHATSAPP_SOURCE, external_id, author, sent_at, body);
    msg.channel = "chat".to_string();
    msg.chat_id = chat_id.to_string();
    msg.raw_json = Value::Object(raw).to_string();
    messages.push(msg);
}

/// Split a quoted-reply body into `(quoted_author, quoted_text, reply)`.
///
/// Layout: the marker line, the quoted reference line (optionally echoed
/// with its full `[stamp] ` header), then the reply text. Returns `None`
/// when the body does not start with a quoted-reply marker.
fn split_quoted(body: &str) -> Option<(Option<String>, String, String)> {
    let mut lines = body.splitn(3, '\n');
    let first = strip_bidi(lines.next()?);
    if !is_quoted_marker(&first) {
        return None;
    }
    let quoted_line = strip_bidi(lines.next().unwrap_or(""));
    let reply = lines.next().unwrap_or("").to_string();

    // Drop an optional echoed header: "[1.11.2026., 09:00:00] Author: x".
    let quoted_line = if quoted_line.starts_with('[') {
        match quoted_line.find(']') {
            Some(close) => quoted_line[close + 1..].trim_start().to_string(),
            None => quoted_line,
        }
    } else {
        quoted_line
    };
    let (quoted_author, quoted_text) = match quoted_line.find(": ") {
        Some(i) => (
            Some(quoted_line[..i].to_string()),
            quoted_line[i + 2..].to_string(),
        ),
        None => (None, quoted_line),
    };
    Some((quoted_author, quoted_text, reply))
}

/// Recognize WhatsApp's attachment placeholders. Media is not extracted in
/// v0: the placeholder text stays searchable and the kind (and name, when
/// present) is recorded for `raw_json`.
fn detect_attachment(body: &str) -> Option<Value> {
    let stripped = strip_bidi(body);
    let text = stripped.trim();

    // "IMG-20261101-WA0001.jpg (file attached)" and friends.
    if let Some(name) = strip_suffix_ci(text, "(file attached)") {
        return Some(json!({ "kind": "file", "name": name.trim() }));
    }
    // "<Media omitted>" — media auto-download was off at export time.
    if text.eq_ignore_ascii_case("<media omitted>") {
        return Some(json!({ "kind": "media" }));
    }
    // "document: plan.pdf omitted".
    if let Some(rest) = strip_prefix_ci(text, "document: ") {
        if let Some(name) = strip_suffix_ci(rest, " omitted") {
            return Some(json!({ "kind": "document", "name": name.trim() }));
        }
    }
    // "image omitted", "video omitted", …
    if let Some(kind_text) = strip_suffix_ci(text, " omitted") {
        let kind = if kind_text.eq_ignore_ascii_case("image") {
            "image"
        } else if kind_text.eq_ignore_ascii_case("video") {
            "video"
        } else if kind_text.eq_ignore_ascii_case("audio") {
            "audio"
        } else if kind_text.eq_ignore_ascii_case("sticker") {
            "sticker"
        } else if kind_text.eq_ignore_ascii_case("gif") {
            "gif"
        } else if kind_text.eq_ignore_ascii_case("contact card") {
            "contact"
        } else {
            return None;
        };
        return Some(json!({ "kind": kind }));
    }
    None
}

/// Case-insensitively strip an ASCII suffix. Byte-safe: an ASCII
/// case-insensitive match always aligns byte-for-byte, so slicing cannot
/// split a multibyte character.
fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    if text.len() >= suffix.len()
        && text.as_bytes()[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
    {
        Some(&text[..text.len() - suffix.len()])
    } else {
        None
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_header_parses_day_first() {
        let h = parse_header("[25.12.2026., 19:04:05] Marko: sretni blagdani", true).unwrap();
        assert_eq!(
            h.datetime.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-12-25T19:04:05"
        );
        assert_eq!(h.author.as_deref(), Some("Marko"));
        assert_eq!(h.body, "sretni blagdani");
    }

    #[test]
    fn bracketed_header_parses_month_first_12h() {
        let h = parse_header("[6/28/23, 2:15:03 PM] Mom: hi", false).unwrap();
        assert_eq!(
            h.datetime.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2023-06-28T14:15:03"
        );
        assert_eq!(h.author.as_deref(), Some("Mom"));
    }

    #[test]
    fn old_android_hyphen_header_parses() {
        let h = parse_header("25.12.26, 19:04 - Marko: hi", true).unwrap();
        assert_eq!(
            h.datetime.format("%Y-%m-%dT%H:%M:%S").to_string(),
            "2026-12-25T19:04:00"
        );
        assert_eq!(h.body, "hi");
    }

    #[test]
    fn system_line_has_no_author() {
        let h = parse_header("[1.11.2026., 09:00:00] Marko je dodao Anu", true).unwrap();
        assert_eq!(h.author, None);
        assert_eq!(h.body, "Marko je dodao Anu");
    }

    #[test]
    fn non_header_lines_are_none() {
        for line in [
            "",
            "plain continuation text",
            "2026 was a year - great one",
            "[not a stamp] nope",
            "25.12.26 has no time - separator",
        ] {
            assert!(parse_header(line, true).is_none(), "{line:?}");
        }
    }

    #[test]
    fn iso_stamp_parses_under_either_order() {
        for day_first in [true, false] {
            let h = parse_header("[2026-11-01, 10:00] Marko: iso", day_first).unwrap();
            assert_eq!(
                h.datetime.format("%Y-%m-%dT%H:%M:%S").to_string(),
                "2026-11-01T10:00:00"
            );
        }
    }

    #[test]
    fn two_digit_year_is_not_year_26_ad() {
        // %Y-first parsing would read "26" as the year 26; the format list
        // tries %y first, so this must be 2026.
        let h = parse_header("[1/11/26, 09:00:00] Marko: hi", true).unwrap();
        assert_eq!(
            h.datetime.format("%Y").to_string(),
            "2026",
            "two-digit year must map to 2026, not 26 AD"
        );
    }

    #[test]
    fn attachment_placeholders_are_recognized() {
        for (body, kind, name) in [
            ("\u{200e}image omitted", "image", None),
            ("Video omitted", "video", None),
            ("audio omitted", "audio", None),
            ("sticker omitted", "sticker", None),
            ("GIF omitted", "gif", None),
            ("Contact card omitted", "contact", None),
            ("\u{200e}<Media omitted>", "media", None),
            ("plan.pdf (file attached)", "file", Some("plan.pdf")),
            ("document: plan.pdf omitted", "document", Some("plan.pdf")),
        ] {
            let att = detect_attachment(body).expect(body);
            assert_eq!(att["kind"], kind, "{body:?}");
            match name {
                Some(n) => assert_eq!(att["name"], n, "{body:?}"),
                None => assert!(att.get("name").is_none(), "{body:?}"),
            }
        }
        // Ordinary text ending in "omitted" is not an attachment.
        assert!(detect_attachment("the word was omitted").is_none());
        assert!(detect_attachment("regular message").is_none());
    }

    #[test]
    fn quoted_body_splits_into_reference_and_reply() {
        let body = "\u{200e}[Citirana poruka]\nJelena: original\ngovorim o arhivi";
        let (author, text, reply) = split_quoted(body).unwrap();
        assert_eq!(author.as_deref(), Some("Jelena"));
        assert_eq!(text, "original");
        assert_eq!(reply, "govorim o arhivi");

        let with_stamp = "[Quoted Message]\n[6/28/23, 9:30:15 AM] Mom: coffee\non my way";
        let (author, text, reply) = split_quoted(with_stamp).unwrap();
        assert_eq!(author.as_deref(), Some("Mom"));
        assert_eq!(text, "coffee");
        assert_eq!(reply, "on my way");
    }

    #[test]
    fn quoted_reference_line_is_not_a_new_message() {
        let export = "\
[6/28/23, 9:31:00 AM] Dad: \u{200e}[Quoted Message]
[6/28/23, 9:30:15 AM] Mom: morning coffee
on my way
[6/28/23, 9:40:00 AM] Mom: thanks
";
        let collected = parse_whatsapp(export.as_bytes(), "us").unwrap();
        assert_eq!(
            collected.messages.len(),
            2,
            "quoted line must not split off"
        );
        let dad = &collected.messages[0];
        assert_eq!(dad.author, "Dad");
        assert_eq!(dad.body, "on my way");
        let raw: Value = serde_json::from_str(&dad.raw_json).unwrap();
        assert_eq!(raw["quoted_author"], "Mom");
        assert_eq!(raw["quoted_text"], "morning coffee");
        // The line after the quote is a real header again.
        assert_eq!(collected.messages[1].author, "Mom");
        assert_eq!(collected.messages[1].body, "thanks");
    }

    #[test]
    fn day_first_detection_uses_file_evidence() {
        let day_first = "Datum\n[25.12.26, 19:04] Marko: hi\n";
        assert!(detect_day_first(day_first));
        let month_first = "[12/25/23, 7:04 PM] Mom: hi\n";
        assert!(!detect_day_first(month_first));
        // Ambiguous files default to day-first.
        assert!(detect_day_first("[11/1/26, 9:00] Marko: hi\n"));
    }

    #[test]
    fn identical_back_to_back_messages_get_distinct_rows() {
        let export = "[1.11.2026., 09:05:00] Marko: ok\n[1.11.2026., 09:05:00] Marko: ok\n";
        let collected = parse_whatsapp(export.as_bytes(), "c").unwrap();
        assert_eq!(collected.messages.len(), 2);
        assert_ne!(
            collected.messages[0].external_id,
            collected.messages[1].external_id
        );
        // Re-parsing is stable, so re-imports dedup.
        let again = parse_whatsapp(export.as_bytes(), "c").unwrap();
        assert_eq!(collected.messages, again.messages);
    }
}
