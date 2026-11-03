//! WhatsApp chat export source: parses the `.txt` file produced by
//! WhatsApp's "Export chat" feature into searchable messages.
//!
//! Supported header shapes (system lines have no author part):
//! - `[1.11.2026., 09:00:05] Marko: Zdravo!` — bracketed; iOS and current
//!   Android exports.
//! - `1.11.2026., 09:00 - Marko: Zdravo!` — older Android exports.
//!
//! Dates are the exporter's local time and carry no UTC offset, so M1.3
//! interprets them in an **explicit timezone** (the importing machine's
//! IANA zone by default) — DST-aware per stamp — and records the zone and
//! a time-confidence label in the canonical store. Local wall-times that
//! a DST transition makes nonexistent stay unresolved (no instant), with
//! the verbatim stamp kept.
//!
//! The `.txt` export carries no chat id, so the conversation key (M1.3) is
//! derived from the export's own content: the sorted participant set
//! (message authors) plus a fingerprint of the first authored message.
//! Re-importing the same chat under a renamed file derives the same key —
//! one conversation, and message ids hash over the key, so renames dedup
//! instead of duplicating. Two different chats that merely share a name
//! (or a participant set) stay distinct because their first messages
//! differ. Exports with no authored message at all cannot be safely keyed
//! and fall back to the legacy file-stem grouping.
//!
//! Messages have no ids in the export; `external_id` is a content hash
//! over the conversation key, author, naive timestamp, and body (see
//! [`parse_whatsapp_tz`]), which keeps re-imports idempotent. Attachment
//! placeholders stay in the body with kind/name recorded in `raw_json`;
//! when the export ships with its files, the media pass (see
//! [`crate::media`]) matches the names and links the stored copies via a
//! `media` array in `raw_json`.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde_json::{json, Map, Value};
use sha2::Digest as _;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a_fields;
use crate::{Error, Result};

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

/// The bounded streaming path (M2.2): three streamed passes over the
/// file — (1) the day/month order from the first 2000 lines, (2) the
/// conversation-key inputs (participant set + first authored message),
/// (3) the messages themselves, one assembled message alive at a time.
/// Every pass holds at most one line plus the pending message being
/// assembled, whatever the export's size. Re-reading is the price of the
/// key derivation (ids hash over the whole chat's identity), and it
/// keeps memory flat instead.
impl StreamSource for WhatsappSource {
    fn name(&self) -> &str {
        WHATSAPP_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let chat_id = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let zone_label = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string());
        stream_whatsapp_tz(
            || Ok(BufReader::new(fs::File::open(&self.path)?)),
            &chat_id,
            &Local,
            &zone_label,
            skip,
            emit,
        )
    }
}

/// Parse WhatsApp export text into messages, interpreting the exporter's
/// naive local timestamps in the machine's local timezone (the IANA name
/// is recorded when resolvable).
pub fn parse_whatsapp(bytes: &[u8], chat_id: &str) -> Result<Collected> {
    let zone_label = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string());
    parse_whatsapp_tz(bytes, chat_id, &Local, &zone_label)
}

/// Parse WhatsApp export text into messages under an explicit timezone.
///
/// `chat_id` is the display grouping (typically the export file's name
/// stem, e.g. `WhatsApp Chat with Ana`) and becomes the conversation
/// label. The conversation key is derived from the export's participants
/// plus its first authored message (see [`whatsapp_conversation_key`]),
/// so it survives file renames. Because the format has no message ids,
/// `external_id` is a content hash over the conversation key (or the
/// legacy stem when no key can be derived), author, naive timestamp, and
/// body, plus an occurrence counter so identical back-to-back messages
/// still get distinct rows — while re-importing the same file (under any
/// name, when a key was derived) dedups everything.
pub fn parse_whatsapp_tz<Tz: TimeZone>(
    bytes: &[u8],
    chat_id: &str,
    tz: &Tz,
    zone_label: &str,
) -> Result<Collected> {
    let mut messages: Vec<NewMessage> = Vec::new();
    let mut failed: u64 = 0;
    stream_whatsapp_tz(
        || Ok(std::io::Cursor::new(bytes)),
        chat_id,
        tz,
        zone_label,
        0,
        &mut |item| {
            match item {
                StreamItem::Message(msg) => messages.push(msg),
                StreamItem::Failed { .. } => failed += 1,
            }
            Ok(Flow::Continue)
        },
    )?;
    Ok(Collected { messages, failed })
}

/// The shared bounded engine behind the whole-file parser and the
/// streaming source: three streamed passes (date order, conversation-key
/// inputs, messages), one assembled message alive at a time. `open` is
/// called once per pass, so any `BufRead` factory works — a file, a
/// cursor over in-memory bytes (the collect path), a test fixture.
pub fn stream_whatsapp_tz<R: BufRead, Tz: TimeZone>(
    open: impl Fn() -> std::io::Result<R>,
    chat_id: &str,
    tz: &Tz,
    zone_label: &str,
    skip: u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
) -> Result<()> {
    // Pass 1: the date order from the file's own leading lines.
    let mut lines = Lines::new(open()?);
    let day_first = detect_day_first_from(&mut lines);
    lines.finish()?;

    // Pass 2: conversation-key inputs — every distinct author plus the
    // first authored message (see [`whatsapp_conversation_key`]).
    let mut authors: BTreeSet<String> = BTreeSet::new();
    let mut anchor: Option<(String, String, String)> = None;
    let mut lines = Lines::new(open()?);
    for_each_finished(&mut lines, day_first, |f| {
        if !f.author.is_empty() {
            authors.insert(f.author.clone());
            if anchor.is_none() {
                anchor = Some((f.author.clone(), f.naive_text.clone(), f.body.clone()));
            }
        }
        true
    });
    lines.finish()?;
    let conversation_key = whatsapp_conversation_key(
        authors.iter().map(String::as_str),
        anchor
            .as_ref()
            .map(|(a, t, b)| (a.as_str(), t.as_str(), b.as_str())),
    );
    // When no key could be derived (no authored message at all) the ids
    // hash over the display stem, exactly as the legacy path did.
    let hash_chat = if conversation_key.is_empty() {
        chat_id
    } else {
        &conversation_key
    };

    // Pass 3: the messages. The occurrence counter advances for skipped
    // items too (conversion happens before the skip check), so a resumed
    // run lands on exactly the same generated ids.
    let mut skip = skip;
    let mut occurrence: HashMap<u64, u32> = HashMap::new();
    let mut lines = Lines::new(open()?);
    let mut failure: Option<Error> = None;
    let mut stopped = false;
    for_each_finished(&mut lines, day_first, |f| {
        if failure.is_some() || stopped {
            return false;
        }
        let msg = finished_to_message(
            f,
            chat_id,
            hash_chat,
            &conversation_key,
            &mut occurrence,
            tz,
            zone_label,
        );
        match crate::bounded::emit_entry(EntryOutcome::Message(msg), &mut skip, emit) {
            Ok(Flow::Continue) => true,
            Ok(Flow::Stop) => {
                stopped = true;
                false
            }
            Err(e) => {
                failure = Some(e);
                false
            }
        }
    });
    lines.finish()?;
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(())
}

/// Lines of an export, streamed: each `read_until` pulls one line, decoded
/// lossily UTF-8 with the trailing `\n`/`\r\n` stripped and a leading BOM
/// dropped from the first line. Per-line lossy decoding equals whole-file
/// lossy decoding split on lines (an invalid UTF-8 sequence can never
/// contain or straddle `\n`), so the streamed and whole-file parsers see
/// identical text. A read error ends the iteration and is held for
/// [`Lines::finish`].
struct Lines<R> {
    reader: R,
    first: bool,
    done: bool,
    error: Option<std::io::Error>,
}

impl<R: BufRead> Lines<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            first: true,
            done: false,
            error: None,
        }
    }

    /// Raise a read error captured during iteration (if any).
    fn finish(&mut self) -> Result<()> {
        match self.error.take() {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    }
}

impl<R: BufRead> Iterator for Lines<R> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        if self.done {
            return None;
        }
        let mut buf = Vec::new();
        match self.reader.read_until(b'\n', &mut buf) {
            Ok(0) => {
                self.done = true;
                return None;
            }
            Ok(_) => {}
            Err(e) => {
                self.error = Some(e);
                self.done = true;
                return None;
            }
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        let mut line = String::from_utf8_lossy(&buf).into_owned();
        if self.first {
            self.first = false;
            if let Some(stripped) = line.strip_prefix('\u{feff}') {
                line = stripped.to_string();
            }
        }
        Some(line)
    }
}

/// The message-assembly state machine, shared by every pass and the
/// whole-file parser: continuation lines join the pending message, a
/// quoted-reply marker consumes the next line as the reference, headers
/// start messages. `f` receives each finished message and returns whether
/// to keep reading (`false` stops the pass promptly).
fn for_each_finished<I, F>(lines: I, day_first: bool, mut f: F)
where
    I: Iterator<Item = String>,
    F: FnMut(Finished) -> bool,
{
    let mut pending: Option<Pending> = None;
    for line in lines {
        // Right after a quoted-reply marker the next line is the quoted
        // reference, even when it looks exactly like a message header
        // (WhatsApp echoes the full `[stamp] Author: text` line).
        let awaiting_quote = pending.as_ref().is_some_and(|p| p.awaiting_quote);
        if !awaiting_quote {
            if let Some(h) = parse_header(&line, day_first) {
                if let Some(p) = pending.take() {
                    if !f(finish_pending(p)) {
                        return;
                    }
                }
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
            p.body.push_str(&line);
            p.awaiting_quote = false;
        }
        // Lines before the first header (BOM leftovers, stray text) are
        // dropped.
    }
    if let Some(p) = pending.take() {
        f(finish_pending(p));
    }
}

/// Turn one assembled message into a [`NewMessage`] (the per-message tail
/// of the whole-file parser, shared with the streaming pass): DST-aware
/// time resolution in the explicit zone and the content-hash id over the
/// stable conversation key, with the occurrence counter keeping identical
/// back-to-back messages distinct.
fn finished_to_message<Tz: TimeZone>(
    f: Finished,
    chat_id: &str,
    hash_chat: &str,
    conversation_key: &str,
    occurrence: &mut HashMap<u64, u32>,
    tz: &Tz,
    zone_label: &str,
) -> NewMessage {
    // DST-aware interpretation in the explicit zone; a wall-time a
    // DST gap removes has no instant at all (kept unresolved).
    let (sent_at, time_confidence) = match local_to_utc(&f.naive, tz) {
        Some(instant) => (
            instant.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "legacy_assumed",
        ),
        None => (String::new(), "unresolved"),
    };

    let base = fnv1a_fields(&[hash_chat, &f.author, &f.naive_text, &f.body]);
    let seen = occurrence.entry(base).or_insert(0);
    let external_id = format!("generated-{:016x}-{}", base, seen);
    *seen += 1;

    let mut msg = NewMessage::new(WHATSAPP_SOURCE, external_id, f.author, sent_at, f.body);
    msg.channel = "chat".to_string();
    msg.chat_id = chat_id.to_string();
    msg.raw_json = Value::Object(f.raw).to_string();
    msg.conversation_key = conversation_key.to_string();
    msg.original_timestamp = f.naive_text.clone();
    msg.timezone = zone_label.to_string();
    msg.time_confidence = time_confidence.to_string();
    msg
}

/// A parsed message with its raw fields ready for storage.
struct Finished {
    /// Stripped author; empty for system lines.
    author: String,
    /// The exporter's naive local stamp, verbatim (`YYYY-MM-DDTHH:MM:SS`).
    naive_text: String,
    naive: NaiveDateTime,
    body: String,
    raw: Map<String, Value>,
}

/// The stable conversation key for a WhatsApp export: the sorted
/// participant set plus a fingerprint of the first authored message,
/// SHA-256-hashed with length-prefixed fields (two exports hash equally
/// only when every field byte matches). Returns `""` when the export has
/// no authored message — nothing in it can then identify the chat, and
/// the caller falls back to the legacy stem grouping.
///
/// Shared by the parser and the M1.3 migration (which re-derives keys for
/// already-imported conversations from the archived rows); the formula is
/// frozen — change it only with a migration that re-keys both sides.
#[doc(alias = "whatsapp_key")]
pub fn whatsapp_conversation_key<'a>(
    authors: impl Iterator<Item = &'a str>,
    anchor: Option<(&'a str, &'a str, &'a str)>,
) -> String {
    let mut authors: Vec<&str> = authors.collect();
    if authors.is_empty() || anchor.is_none() {
        return String::new();
    }
    authors.sort_unstable();
    authors.dedup();
    let Some((anchor_author, anchor_stamp, anchor_body)) = anchor else {
        unreachable!("checked above");
    };
    let mut hasher = sha2::Sha256::new();
    for part in authors
        .iter()
        .copied()
        .chain([anchor_author, anchor_stamp, anchor_body])
    {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut key = String::with_capacity("whatsapp:chat:sha256:".len() + 64);
    key.push_str("whatsapp:chat:sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
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

/// [`is_whatsapp_export`] streamed from a reader: only the first 30 lines
/// are read, so detection of an arbitrarily large file stays bounded
/// (the whole-file `decode_export` variant remains for tests and small
/// buffers).
pub fn is_whatsapp_export_reader<R: std::io::Read>(reader: R) -> bool {
    let mut lines = Lines::new(BufReader::new(reader));
    (&mut lines)
        .take(30)
        .any(|line| parse_header(&line, true).is_some() || parse_header(&line, false).is_some())
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
/// [`detect_day_first`] over a whole decoded text — the test-facing
/// spelling (the parsers themselves use the streamed variant).
#[cfg(test)]
fn detect_day_first(text: &str) -> bool {
    detect_day_first_from(text.lines().map(str::to_string))
}

/// [`detect_day_first`] over streamed lines (only the first 2000 are
/// consulted, mirroring the whole-file parser).
fn detect_day_first_from<I: Iterator<Item = String>>(lines: I) -> bool {
    let mut day_first_seen = false;
    let mut month_first_seen = false;
    for line in lines.take(2000) {
        if let Some(token) = leading_date_token(&line) {
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

fn local_to_utc<Tz: TimeZone>(naive: &NaiveDateTime, tz: &Tz) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        // A DST fold repeats the wall-time: the first occurrence, the
        // same choice chrono's `earliest()` makes everywhere else.
        chrono::LocalResult::Ambiguous(dt, _) => Some(dt.with_timezone(&Utc)),
        // A DST gap removed this wall-time from the calendar: no instant
        // exists to resolve to.
        chrono::LocalResult::None => None,
    }
}

fn is_quoted_marker(first_body_line: &str) -> bool {
    QUOTED_MARKERS.contains(&first_body_line)
}

/// Turn a finished [`Pending`] into a [`Finished`] (the conversation key
/// and id hashing happen later, once the whole export has been seen).
fn finish_pending(p: Pending) -> Finished {
    let author = p.author.map(|a| strip_bidi(&a)).unwrap_or_default();
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

    Finished {
        author,
        naive_text: p.datetime.format("%Y-%m-%dT%H:%M:%S").to_string(),
        naive: p.datetime,
        body,
        raw,
    }
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

/// Recognize WhatsApp's attachment placeholders. The placeholder text
/// stays searchable and the kind (and name, when present) is recorded
/// for `raw_json`; when the export ships with its files, the media pass
/// (see [`crate::media`]) matches named attachments and links the stored
/// copies via a `media` array in `raw_json`.
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
