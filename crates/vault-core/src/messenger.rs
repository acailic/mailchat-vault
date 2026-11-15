//! Facebook Messenger (DYI JSON) export source: parses the
//! `messages/inbox/<thread>/message_*.json` files written by
//! "Download Your Information" with format set to JSON.
//!
//! Every original message object is kept verbatim in `raw_json` (text
//! and media references alike); the conversation key comes from the
//! thread folder's numeric suffix; and messages carry synthetic
//! external ids (Facebook exports none). A pick may be one thread file,
//! a thread folder (its `message_1.json` … `message_N.json` parts
//! merge in numeric order), or any DYI directory above them (the export
//! root, `messages/`, `messages/inbox/`): the walk in this module finds
//! the thread folders and merges each thread under one shared context.
//!
//! The file is read with the same two-pass streaming as the other chat
//! JSON sources (pass 1: the header; pass 2: `messages`, one entry at a
//! time). Detection ([`is_messenger_export`]) runs ahead of the
//! Telegram importer's probe, whose format test is merely "has a
//! `messages` array" — [`crate::chat_export`] pins the detection order.
//!
//! A part file that cannot be read or parsed never kills the pick: it
//! becomes one [`crate::bounded::StreamItem::Failed`] item naming the
//! file and the walk continues with the next part — the same
//! skip-and-record rule a broken `.eml` gets in an email folder (see
//! [`crate::mbox::EmlDirSource`]). Only structural refusals (a pick
//! with no thread folders, an unreadable directory) fail the pick, and
//! those errors name the offending path.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Deserializer as _};
use serde_json::Value;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a_fields;
use crate::{Error, Result};

/// Source name and dedup namespace for Messenger imports.
pub const MESSENGER_SOURCE: &str = "messenger";

/// Detection-only walk of a DYI message file (streamed: only the first
/// `messages` element is materialized, `participants` elements are
/// zero-sized `IgnoredAny`s), so detecting an arbitrarily large export
/// stays bounded. True only for an object with a `participants` array
/// plus a `messages` array whose first element carries `sender_name` or
/// `timestamp_ms` — the combination no other supported format has.
pub fn is_messenger_export(bytes: &[u8]) -> bool {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

/// [`is_messenger_export`] streamed from a reader (the caller rewinds
/// between probes, as `chat_export::detect_chat_format_reader` does).
pub fn is_messenger_export_reader<R: std::io::Read>(reader: R) -> bool {
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(reader));
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

struct DetectVisitor;

impl<'de> serde::de::Visitor<'de> for DetectVisitor {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Messenger export object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<bool, A::Error> {
        // The whole map is walked (every other member is a zero-sized
        // `IgnoredAny`): serde_json checks the container's end even
        // after an early visitor return, so a mid-map `return Ok(true)`
        // would surface as a parse error and read as `false`.
        let mut participants = false;
        let mut sender_shaped = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "participants" => {
                    let _ = map.next_value::<Vec<serde::de::IgnoredAny>>()?; // array-ness
                    participants = true;
                }
                "messages" => {
                    sender_shaped = map.next_value_seed(FirstElementSender)?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(participants && sender_shaped)
    }
}

/// Reads only the first element of the `messages` array and reports
/// whether it is a Messenger message object.
struct FirstElementSender;

impl<'de> serde::de::DeserializeSeed<'de> for FirstElementSender {
    type Value = bool;

    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<bool, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for FirstElementSender {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a messages array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<bool, A::Error> {
        let Some(first) = seq.next_element::<Value>()? else {
            return Ok(false);
        };
        // The rest of the array is drained token by token through
        // zero-sized `IgnoredAny`s — bounded, and required: serde_json
        // errors on a visitor that stops mid-sequence.
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        let Some(obj) = first.as_object() else {
            return Ok(false);
        };
        Ok(obj.contains_key("sender_name") || obj.contains_key("timestamp_ms"))
    }
}

/// Reads one Messenger DYI thread file (`message_*.json`) and yields
/// its messages.
#[derive(Debug, Clone)]
pub struct MessengerSource {
    path: PathBuf,
}

impl MessengerSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for MessengerSource {
    fn name(&self) -> &str {
        MESSENGER_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        // The whole-archive convenience view of the same streaming walk
        // the bounded driver runs (one code path, so `collect` and
        // `stream` cannot drift apart — including on broken parts).
        let mut messages = Vec::new();
        let mut failed = 0u64;
        let mut failure = None;
        {
            let mut state = crate::bounded::ConsumerState {
                skip: 0,
                emit: &mut |item: StreamItem| {
                    match item {
                        StreamItem::Message(msg) => messages.push(msg),
                        StreamItem::Failed { .. } => failed += 1,
                    }
                    Ok(Flow::Continue)
                },
                stopped: false,
                failure: &mut failure,
            };
            if self.path.is_file() {
                stream_thread(
                    std::slice::from_ref(&self.path),
                    &parent_folder_name(&self.path),
                    &mut state,
                )?;
            } else {
                stream_tree(&self.path, &mut state)?;
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        Ok(Collected { messages, failed })
    }
}

/// The bounded streaming path (M2.2): every file is read in two
/// streamed passes — pass 1 reads only the header members (`title`,
/// `thread_path`, `participants`; the `messages` array is skipped token
/// by token through zero-sized `IgnoredAny`s), pass 2 walks the
/// `messages` array one element at a time. A single-file pick runs both
/// passes over the file; a directory pick runs the header pass once per
/// thread (on the numerically first part that parses — one context
/// covers the whole thread) and the messages pass over every part,
/// threads in path order. Peak memory is one message entry plus the
/// small header and the per-thread occurrence counter, whatever the
/// tree's size. A part that cannot be read or parsed yields one
/// `Failed` item naming the file and the walk continues (see
/// [`stream_thread`]); a recorded failure is a committed item, so the
/// bounded checkpoint can always advance past a corrupt part.
impl StreamSource for MessengerSource {
    fn name(&self) -> &str {
        MESSENGER_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let mut failure = None;
        let result = {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            if self.path.is_file() {
                stream_thread(
                    std::slice::from_ref(&self.path),
                    &parent_folder_name(&self.path),
                    &mut state,
                )
            } else {
                stream_tree(&self.path, &mut state)
            }
        };
        if let Some(e) = failure {
            return Err(e);
        }
        result
    }
}

/// True when `name` is a DYI message file: `message_<number>.json`.
/// (`pub(crate)`: the folder watcher routes settled part files to
/// their thread with the same rule.)
pub(crate) fn is_message_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("message_")
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// The numeric suffix of `message_<n>.json` (files without one sort last).
fn message_number(path: &Path) -> u64 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit_once('_'))
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(u64::MAX)
}

/// Thread directories under a Messenger pick: every directory that
/// directly holds `message_*.json` files, found by a depth-capped walk —
/// the pick may be the DYI root, `messages/`, `messages/inbox/`, or a
/// single thread folder. A directory with its own message files never
/// descends (thread folders hold only their own parts). Hidden and
/// macOS-junk entries never count (the shared fence rule).
fn thread_dirs(root: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut has_own = false;
    let mut subdirs = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| read_failed(root, e))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') || name == "__MACOSX" || name.starts_with("._") {
            continue;
        }
        if path.is_dir() {
            subdirs.push(path);
        } else if is_message_file(&path) {
            has_own = true;
        }
    }
    if has_own {
        out.push(root.to_path_buf());
        return Ok(());
    }
    if depth == 0 {
        return Ok(());
    }
    subdirs.sort();
    for dir in subdirs {
        thread_dirs(&dir, depth - 1, out)?;
    }
    Ok(())
}

/// The message files of one thread in numeric order (`message_2` before
/// `message_10`).
fn thread_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| read_failed(dir, e))? {
        let path = entry?.path();
        if path.is_file() && is_message_file(&path) {
            files.push(path);
        }
    }
    files.sort_by_key(|f| (message_number(f), f.to_path_buf()));
    Ok(files)
}

/// The thread folders of a directory pick, in path order; refuses a pick
/// with none (the Discord spelling: InvalidInput, naming the path).
fn thread_pick(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    thread_dirs(root, 6, &mut dirs)?;
    if dirs.is_empty() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "no Messenger message_*.json files found under {}",
                root.display()
            ),
        )));
    }
    Ok(dirs)
}

/// True when the tree holds at least one Messenger thread directory.
pub fn is_messenger_export_dir(root: &Path) -> bool {
    let mut dirs = Vec::new();
    thread_dirs(root, 6, &mut dirs).is_ok() && !dirs.is_empty()
}

/// The DYI root a Messenger tree pick resolves media against: the pick
/// itself when it holds `messages/`, else the nearest ancestor that
/// does (a `messages/`, `inbox/`, or thread-folder pick climbs back
/// up), capped at three levels. Falls back to the pick itself.
pub fn messenger_export_root(pick: &Path) -> PathBuf {
    let mut cur = pick.to_path_buf();
    for _ in 0..4 {
        if cur.join("messages").is_dir() {
            return cur;
        }
        match cur.parent() {
            Some(parent) => cur = parent.to_path_buf(),
            None => break,
        }
    }
    pick.to_path_buf()
}

/// The thread folder's own name — the same hint [`parent_folder_name`]
/// derives for a single file pick, taken from the folder itself.
fn thread_folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// An unreadable directory aborts the walk; the error names the path so
/// the bad pick can be found (the Discord `read_failed` spelling).
fn read_failed(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// Stream a directory pick: every thread in path order, each thread's
/// parts in numeric order, one shared occurrence map per thread (the
/// single code path both `collect` and the bounded driver run). A
/// consumer stop or failure ends the whole pick — the occurrence count
/// must not resume mid-thread on a later part.
fn stream_tree(root: &Path, state: &mut crate::bounded::ConsumerState<'_>) -> Result<()> {
    let threads = thread_pick(root)?;
    for thread in &threads {
        let files = thread_files(thread)?;
        stream_thread(&files, &thread_folder_name(thread), state)?;
        if state.stopped || state.failure.is_some() {
            break;
        }
    }
    Ok(())
}

/// Stream one thread (a single picked file, or one thread's parts in
/// numeric order) with the `.eml` folder's skip-and-record rule: a part
/// file that cannot be read or parsed is one `Failed` item whose
/// `external_ref` is the file path and whose reason carries the
/// underlying error; the walk continues with the next file. Messages
/// already emitted from a part that fails mid-array stay emitted,
/// before that part's failure item — the same bytes always yield the
/// same item sequence, so skip-based resume replays identically.
///
/// The header pass reads parts in numeric order until one parses, so a
/// broken first part costs its thread one failure item, not the
/// thread's context; a thread with no parseable part at all is one
/// `Failed` item naming the first part.
fn stream_thread(
    files: &[PathBuf],
    folder_stem: &str,
    state: &mut crate::bounded::ConsumerState<'_>,
) -> Result<()> {
    let Some(first) = files.first() else {
        return Ok(());
    };
    // Pass 1 (parts in numeric order until one parses): the header,
    // streamed — the messages array costs no memory (IgnoredAny
    // elements are zero-sized).
    let mut ctx: Option<ThreadContext> = None;
    let mut first_reason: Option<String> = None;
    for file in files {
        match read_thread_header(file) {
            Ok(header) => {
                ctx = Some(ThreadContext::from_header(&header, folder_stem));
                break;
            }
            Err(reason) => {
                first_reason.get_or_insert(reason);
            }
        }
    }
    let Some(ctx) = ctx else {
        // No part parses at all: the thread is one failure naming its
        // first part — there is no context to walk, so no per-file
        // breakdown to report.
        let _ = state.offer(EntryOutcome::Failed {
            external_ref: first.display().to_string(),
            reason: first_reason.unwrap_or_else(|| "unreadable or unparseable part".to_string()),
        });
        return Ok(());
    };
    let mut occ: HashMap<u64, u32> = HashMap::new();
    for file in files {
        // Pass 2: the messages array, one element at a time. The
        // occurrence map is shared by every part of the thread: ids must
        // count repeats across the whole thread, not one file.
        let result = {
            let visitor = MessagesVisitor {
                ctx: &ctx,
                occ: &mut occ,
                state,
            };
            match fs::File::open(file) {
                Ok(open) => serde_json::Deserializer::from_reader(BufReader::new(open))
                    .deserialize_map(visitor),
                // The same one-failure-per-part treatment an unparseable
                // part gets (the reason names the underlying error).
                Err(e) => Err(serde_json::Error::io(e)),
            }
        };
        if let Some(e) = state.failure.take() {
            return Err(e);
        }
        match result {
            Ok(()) => {}
            Err(e) if state.stopped && crate::bounded::ConsumerState::is_stop_sentinel(&e) => {
                return Ok(())
            }
            Err(e) => {
                // The part's own failure item, offered through the
                // shared state so a resumed run's skip counter replays
                // the same sequence.
                if !state.offer(EntryOutcome::Failed {
                    external_ref: file.display().to_string(),
                    reason: format!("unreadable or unparseable part: {e}"),
                }) {
                    if let Some(e) = state.failure.take() {
                        return Err(e);
                    }
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Pass 1 of one part: its header, streamed. The error string names the
/// underlying read or parse error for the failure-item path.
fn read_thread_header(file: &Path) -> std::result::Result<MessengerHeader, String> {
    let opened = fs::File::open(file).map_err(|e| format!("unreadable part: {e}"))?;
    serde_json::from_reader(BufReader::new(opened)).map_err(|e| format!("unparseable part: {e}"))
}

/// The header view of one thread file: `title`/`thread_path`/
/// `participants` are read small; `messages` is enforced to be an array
/// and skipped element by element (zero-sized `IgnoredAny`s).
#[derive(Debug, Deserialize)]
struct MessengerHeader {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    thread_path: Option<String>,
    #[serde(default)]
    participants: Vec<ParticipantName>,
    // The field exists to make serde enforce array-ness (the same trick
    // as `TelegramHeader`); IgnoredAny elements are zero-sized, so a
    // huge messages array costs no memory proportional to its size.
    // Never read — pass 2 walks the messages streamed. Defaulted: a
    // part file without the member parses (empty), the same tolerance
    // the whole-file parser's defaulted `MessengerExport.messages` has.
    #[serde(default)]
    #[allow(dead_code)]
    messages: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct ParticipantName {
    #[serde(default)]
    name: Option<String>,
}

/// Conversation context shared by every entry of one thread: display
/// grouping, stable dedup thread key, and the M1.3 conversation key.
struct ThreadContext {
    chat_id: String,
    thread_key: String,
    conversation_key: String,
}

impl ThreadContext {
    fn from_header(header: &MessengerHeader, folder_stem: &str) -> Self {
        let participants = header
            .participants
            .iter()
            .filter_map(|p| p.name.as_deref().filter(|n| !n.is_empty()))
            .collect::<Vec<_>>()
            .join(", ");
        // Display grouping: the export's title, else the participant
        // list, else the folder name.
        let chat_id = header
            .title
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| (!participants.is_empty()).then(|| participants.clone()))
            .unwrap_or_else(|| folder_stem.to_string());
        let thread_key = thread_id(header.thread_path.as_deref(), folder_stem);
        let conversation_key = if thread_key.is_empty() {
            String::new()
        } else {
            format!("messenger:thread:{thread_key}")
        };
        Self {
            chat_id,
            thread_key,
            conversation_key,
        }
    }
}

/// The stable thread id: the trailing `_<digits>` group of the thread
/// folder name, from `thread_path` (e.g. `inbox/ana_1234567890`) or the
/// folder stem — whichever carries digits. Empty when neither does
/// (a hand-trimmed export): legacy chat_id grouping applies.
fn thread_id(thread_path: Option<&str>, folder_stem: &str) -> String {
    for candidate in [
        thread_path.unwrap_or("").rsplit('/').next().unwrap_or(""),
        folder_stem,
    ] {
        if let Some(pos) = candidate.rfind('_') {
            let digits = &candidate[pos + 1..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return digits.to_string();
            }
        }
    }
    String::new()
}

/// The thread folder name a single `message_*.json` file sits in
/// (`…/<thread folder>/message_1.json` → the folder's name): the
/// fallback hint for `chat_id` and the thread id when the header's
/// `thread_path` carries neither. Empty at the filesystem root.
fn parent_folder_name(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fields whose presence makes an entry a media message even without
/// `content` (Telegram's service-entry skip, scoped to chat metadata).
const MEDIA_FIELDS: [&str; 6] = [
    "photos",
    "videos",
    "audio_files",
    "files",
    "gifs",
    "sticker",
];

/// Convert one `messages` array entry through the shared mapping (used
/// by both the whole-file parser and the streaming pass, so their
/// behavior cannot drift apart).
fn messenger_entry(
    entry: &Value,
    ctx: &ThreadContext,
    occurrence: &mut HashMap<u64, u32>,
) -> EntryOutcome {
    let Some(obj) = entry.as_object() else {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "entry is not a JSON object".to_string(),
        };
    };
    let body = obj
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let has_media = MEDIA_FIELDS.iter().any(|f| {
        obj.get(*f).is_some_and(|v| match v {
            Value::Array(a) => !a.is_empty(),
            Value::Object(_) => true,
            _ => false,
        })
    });
    let sender = obj.get("sender_name").and_then(Value::as_str);
    let ts = obj.get("timestamp_ms").and_then(Value::as_i64);
    if sender.is_none() && ts.is_none() {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "entry is not a Messenger message".to_string(),
        };
    }
    if body.is_empty() && !has_media {
        return EntryOutcome::Skip;
    }
    let author = sender.unwrap_or_default().to_string();
    let ts_text = ts.map(|t| t.to_string()).unwrap_or_default();
    let sent_at = ts
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    // FB exports carry no message ids: a content hash over the stable
    // thread key + author + timestamp + body, with an occurrence counter
    // for byte-identical repeats (the WhatsApp formula).
    let base = fnv1a_fields(&[&ctx.thread_key, &author, &ts_text, &body]);
    let seen = occurrence.entry(base).or_insert(0);
    let external_id = format!("generated-{base:016x}-{seen}");
    *seen += 1;

    let mut msg = NewMessage::new(MESSENGER_SOURCE, external_id, author, sent_at, body);
    msg.channel = "chat".to_string();
    msg.chat_id = ctx.chat_id.clone();
    msg.conversation_key = ctx.conversation_key.clone();
    // The whole original object: media references, reactions, share
    // metadata — all stay recorded verbatim.
    msg.raw_json = entry.to_string();
    msg.original_timestamp = ts_text;
    msg.timezone = "UTC".to_string();
    msg.time_confidence = if ts.is_some() {
        "explicit_offset"
    } else {
        "missing"
    }
    .to_string();
    EntryOutcome::Message(msg)
}

/// serde visitor of the streaming pass: walks the top-level object,
/// streaming the `messages` array element by element; every other member
/// is skipped. Stops reading as soon as the consumer stops or fails (the
/// deserializer is used directly, without an end-of-input check, so an
/// early return leaves the reader where it is — nothing else is read).
struct MessagesVisitor<'a, 'c> {
    ctx: &'a ThreadContext,
    occ: &'a mut HashMap<u64, u32>,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::Visitor<'de> for MessagesVisitor<'a, 'c> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Messenger thread object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        // A duplicate top-level `messages` key streams only the first
        // array; later occurrences are drained through the ignore arm
        // (serde_json requires the value consumed before the map can
        // end). The whole-file parser keeps its own standard serde
        // duplicate-key handling — the paths intentionally diverge on
        // that pathological shape.
        let mut messages_seen = false;
        while let Some(key) = map.next_key::<String>()? {
            if key == "messages" && !messages_seen {
                messages_seen = true;
                map.next_value_seed(MessagesSeed {
                    ctx: self.ctx,
                    occ: self.occ,
                    state: self.state,
                })?;
                // The messages were streamed; members after them (DYI
                // files order their members freely) are drained by this
                // loop's ignore arm — an early return would leave the
                // walk mid-map and fail serde_json's end-of-container
                // scan.
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// Seed that streams the `messages` array: each element is parsed into a
/// `Value` on its own (one entry alive at a time) and offered to the
/// consumer.
struct MessagesSeed<'a, 'c> {
    ctx: &'a ThreadContext,
    occ: &'a mut HashMap<u64, u32>,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::DeserializeSeed<'de> for MessagesSeed<'a, 'c> {
    type Value = ();

    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, 'a, 'c> serde::de::Visitor<'de> for MessagesSeed<'a, 'c> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a messages array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<(), A::Error> {
        while let Some(entry) = seq.next_element::<Value>()? {
            let outcome = messenger_entry(&entry, self.ctx, self.occ);
            if !self.state.offer(outcome) {
                return Err(crate::bounded::ConsumerState::stop());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_bytes() -> Vec<u8> {
        br#"{"participants": [{"name": "Ana Peric"}], "messages": [{"sender_name": "Ana Peric", "timestamp_ms": 1762981200000, "content": "Zdravo!"}], "title": "Ana Peric", "thread_path": "inbox/anaperic_1234567890"}"#.to_vec()
    }

    #[test]
    fn detects_messenger_thread_json() {
        assert!(is_messenger_export(&thread_bytes()));
    }

    #[test]
    fn detects_thread_with_many_messages_and_mixed_key_order() {
        // A real DYI file has thousands of messages and keys around the
        // two probes care about; only the first element materializes.
        let reversed = br#"{"title": "Ana Peric", "messages": [{"timestamp_ms": 2, "sender_name": "Ana", "content": "druga"}, {"sender_name": "Ana", "timestamp_ms": 3, "content": "treca"}], "participants": [{"name": "Ana"}]}"#;
        assert!(is_messenger_export(reversed));
        let rest: Vec<String> = (2..=51)
            .map(|i| format!("{{\"sender_name\": \"Ana\", \"timestamp_ms\": {i}}}"))
            .collect();
        let many = format!(
            "{{\"participants\": [], \"messages\": [{{\"sender_name\": \"Ana\", \"timestamp_ms\": 1}}, {}]}}",
            rest.join(", ")
        );
        assert!(is_messenger_export(many.as_bytes()));
    }

    #[test]
    fn telegram_shape_is_not_messenger() {
        // Telegram: messages array but no participants array / sender_name.
        assert!(!is_messenger_export(
            br#"{"name": "x", "id": 42, "messages": [{"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "text": "hi"}]}"#
        ));
        assert!(!is_messenger_export(br#"{"messages": []}"#));
    }

    #[test]
    fn streaming_tolerates_members_after_messages() {
        // DYI files order their members freely: `messages` may come
        // first. The streamed pass must drain what follows instead of
        // failing on it, and still match the whole-file parser.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message_1.json");
        std::fs::write(
            &path,
            br#"{"messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "prva"}, {"sender_name": "Ana", "timestamp_ms": 1762981260000, "content": "druga"}], "participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111", "magic_words": []}"#,
        )
        .unwrap();
        let collected = MessengerSource::new(&path).collect().unwrap();
        assert_eq!(collected.messages.len(), 2);
        let mut streamed = Vec::new();
        MessengerSource::new(&path)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        assert_eq!(streamed.len(), 2);
        for (item, msg) in streamed.iter().zip(&collected.messages) {
            assert_eq!(item, &StreamItem::Message(msg.clone()));
        }
    }

    #[test]
    fn missing_messages_member_is_tolerated_on_both_paths() {
        // A part file without a `messages` member must not fail the
        // whole streaming pick: the header defaults it to empty, so
        // `collect` succeeds with zero messages and `stream` yields
        // nothing without error — the paths agree.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message_1.json");
        std::fs::write(
            &path,
            br#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111"}"#,
        )
        .unwrap();
        let collected = MessengerSource::new(&path).collect().unwrap();
        assert_eq!(collected.messages.len(), 0);
        assert_eq!(collected.failed, 0);
        let mut streamed = Vec::new();
        MessengerSource::new(&path)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        assert_eq!(streamed.len(), 0);
    }

    #[test]
    fn duplicate_messages_key_streams_only_the_first_array() {
        // A duplicated top-level `messages` key in a later part file:
        // pass 1 reads only the numerically first part, so nothing
        // fails the pick, and the guard makes pass 2 stream just the
        // first array — before the guard, both arrays streamed. There
        // is no whole-file parser anymore (`collect` runs this same
        // visitor), so no divergence note is needed on this shape.
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(
            thread.join("message_1.json"),
            br#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111", "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981140000, "content": "nula"}]}"#,
        )
        .unwrap();
        // This part holds two distinct `messages` arrays.
        std::fs::write(
            thread.join("message_2.json"),
            br#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "prva"}, {"sender_name": "Ana", "timestamp_ms": 1762981260000, "content": "druga"}], "title": "Ana", "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981320000, "content": "treca"}]}"#,
        )
        .unwrap();
        let mut streamed = Vec::new();
        MessengerSource::new(&thread)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        let bodies: Vec<&str> = streamed
            .iter()
            .map(|item| match item {
                StreamItem::Message(m) => m.body.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(bodies, ["nula", "prva", "druga"]);
    }

    #[test]
    fn message_files_sort_by_number() {
        assert!(is_message_file(Path::new("t/message_1.json")));
        assert!(is_message_file(Path::new("t/message_12.JSON")));
        assert!(!is_message_file(Path::new("t/._message_1.json")));
        assert!(!is_message_file(Path::new("t/message_1.txt")));
        assert!(!is_message_file(Path::new("t/photo.json")));
        assert_eq!(message_number(Path::new("t/message_2.json")), 2);
        assert_eq!(message_number(Path::new("t/message_10.json")), 10);
        // No numeric suffix: sorts last.
        assert_eq!(message_number(Path::new("t/message_x.json")), u64::MAX);
    }

    #[test]
    fn thread_walk_is_depth_capped() {
        // Seven levels below the pick is past the cap of six.
        let dir = tempfile::tempdir().unwrap();
        let within = dir.path().join("a/b/c/d/e/f");
        let beyond = dir.path().join("g/h/i/j/k/l/m");
        for thread in [&within, &beyond] {
            std::fs::create_dir_all(thread).unwrap();
            std::fs::write(thread.join("message_1.json"), thread_bytes()).unwrap();
        }
        let mut dirs = Vec::new();
        thread_dirs(dir.path(), 6, &mut dirs).unwrap();
        assert_eq!(dirs, [within]);
    }

    #[test]
    fn thread_folders_never_descend() {
        // A folder with its own message files is a thread; a nested
        // folder holding more is not walked.
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("inbox/ana_1111");
        let nested = thread.join("old/ana_9999");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(thread.join("message_1.json"), thread_bytes()).unwrap();
        std::fs::write(nested.join("message_1.json"), thread_bytes()).unwrap();
        let mut dirs = Vec::new();
        thread_dirs(dir.path(), 6, &mut dirs).unwrap();
        assert_eq!(dirs, [thread]);
        assert!(is_messenger_export_dir(dir.path()));
        assert!(!is_messenger_export_dir(
            &dir.path().join("inbox/ana_1111/old/missing")
        ));
    }

    #[test]
    fn export_root_climbs_to_the_messages_holder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("facebook-ana");
        let thread = root.join("messages/inbox/ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        for pick in [
            root.clone(),
            root.join("messages"),
            root.join("messages/inbox"),
            thread.clone(),
        ] {
            assert_eq!(messenger_export_root(&pick), root, "{}", pick.display());
        }
        // No `messages/` holder within three levels: the pick itself.
        let loose = dir.path().join("x/y/z/w/ana_1111");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(messenger_export_root(&loose), loose);
    }

    #[test]
    fn broken_json_is_one_failed_item_naming_the_file() {
        // Skip-and-record: an unparseable part — even the only file of
        // the pick — is one `Failed` item naming it, never a fatal
        // error (the `.eml` folder rule). `collect` runs the same walk,
        // so the paths agree.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message_1.json");
        std::fs::write(&path, b"{not json").unwrap();
        let collected = MessengerSource::new(&path).collect().unwrap();
        assert_eq!(collected.messages.len(), 0);
        assert_eq!(collected.failed, 1);
        let mut streamed = Vec::new();
        MessengerSource::new(&path)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        match &streamed[..] {
            [StreamItem::Failed {
                external_ref,
                reason,
            }] => {
                assert!(external_ref.ends_with("message_1.json"), "{external_ref}");
                assert!(!reason.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn thread_with_no_parseable_part_is_one_failed_item_naming_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(thread.join("message_1.json"), b"{not json").unwrap();
        std::fs::write(thread.join("message_2.json"), b"also broken").unwrap();
        let mut streamed = Vec::new();
        MessengerSource::new(&thread)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        // One item for the whole thread, naming the numerically first
        // part (its own underlying error in the reason).
        match &streamed[..] {
            [StreamItem::Failed {
                external_ref,
                reason,
            }] => {
                assert!(external_ref.ends_with("message_1.json"), "{external_ref}");
                assert!(reason.contains("unparseable part"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        let collected = MessengerSource::new(&thread).collect().unwrap();
        assert_eq!(collected.messages.len(), 0);
        assert_eq!(collected.failed, 1);
    }

    #[test]
    fn mid_array_failure_keeps_emitted_messages_then_records_the_part() {
        // A part truncated mid-array (the header comes from the valid
        // first part): the entries before the cut stay emitted,
        // followed by the broken part's one failure item —
        // deterministic for the same bytes, so skip-based resume
        // replays identically.
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(
            thread.join("message_1.json"),
            br#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111"}"#,
        )
        .unwrap();
        std::fs::write(
            thread.join("message_2.json"),
            br#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "cela"}, {"sender_name": "Ana", "timestamp_ms""#,
        )
        .unwrap();
        let mut streamed = Vec::new();
        MessengerSource::new(&thread)
            .stream(0, &mut |item| {
                streamed.push(item);
                Ok(Flow::Continue)
            })
            .unwrap();
        match &streamed[..] {
            [StreamItem::Message(m), StreamItem::Failed { external_ref, .. }] => {
                assert_eq!(m.body, "cela");
                assert!(external_ref.ends_with("message_2.json"), "{external_ref}");
            }
            other => panic!("{other:?}"),
        }
    }
}
