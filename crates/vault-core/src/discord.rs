//! Discord chat export source: parses the per-channel `messages.json`
//! files written by Discord's "Request Data" package.
//!
//! A Discord Data Package keeps every archived channel as one JSON file
//! under `messages/` (named by channel id), plus an `index.json` mapping
//! channel ids to human-readable paths. Guild channels carry a `guild`
//! object; DMs do not. Each message object holds a snowflake `id`
//! (globally unique), a `timestamp` in `YYYY-MM-DD HH:MM:SS.mmm+00:00`
//! form, the `content` text, and structured `attachments` / `reactions` /
//! `embeds` arrays.
//!
//! v0 archives text: attachments are not extracted, but the whole
//! original message object is kept verbatim in `raw_json`, so reactions,
//! attachment metadata, edited timestamps, and mentions stay recorded.
//! System entries (joins, pins, calls — any `type` other than the
//! user-content kinds) carry no body and are skipped. Timestamps are
//! normalized to UTC. The import path may be one channel file, the
//! `messages/` directory itself, or the package root (its `messages/`
//! subdirectory is used when present).

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer as _};
use serde_json::Value;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::{Error, Result};

/// Source name and dedup namespace for Discord imports.
pub const DISCORD_SOURCE: &str = "discord";

/// Message `type` values that carry user content. Everything else
/// ("GuildMemberJoin", "RecipientAdd", "ChannelPinnedMessage", "Call", …)
/// is a system entry with no body and is skipped, like Telegram's service
/// entries. "Reply" and the application-command kinds are real messages.
const CONTENT_TYPES: [&str; 4] = ["Default", "Reply", "ChatInputCommand", "ContextMenuCommand"];

/// Reads Discord package messages: one channel `messages.json`, a
/// directory of them, or a package root containing `messages/`.
#[derive(Debug, Clone)]
pub struct DiscordSource {
    path: PathBuf,
}

impl DiscordSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for DiscordSource {
    fn name(&self) -> &str {
        DISCORD_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        if self.path.is_file() {
            // A single channel file: its sibling index.json (when picked
            // straight out of a package's messages/ dir) still resolves
            // DM chat names.
            let index = index_map(self.path.parent().unwrap_or(Path::new("")));
            let fallback = file_stem_hint(&self.path);
            let bytes = fs::read(&self.path)?;
            return parse_discord_channel(&bytes, &index, &fallback);
        }
        import_directory(&self.path)
    }
}

/// The bounded streaming path (M2.2): channel files are processed one at
/// a time (sorted, as the directory import is), and each file is read in
/// two streamed passes — its header (`channel`/`guild`, with the
/// `messages` array skipped through zero-sized `IgnoredAny`s) and then
/// its `messages` array, one element alive at a time. Peak memory is one
/// message entry plus one channel's header, whatever the package's size.
impl StreamSource for DiscordSource {
    fn name(&self) -> &str {
        DISCORD_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let mut failure = None;
        {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            if self.path.is_file() {
                let index = index_map(self.path.parent().unwrap_or(Path::new("")));
                let fallback = file_stem_hint(&self.path);
                stream_channel_file(&self.path, &index, &fallback, &mut state)?;
            } else {
                let files = channel_files(&self.path)?;
                if files.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "no Discord channel .json files found under {} \
                             (expected the package's messages/ directory or its parent)",
                            self.path.display()
                        ),
                    )
                    .into());
                }
                let index = index_map(&self.path);
                for file in &files {
                    let fallback = file_stem_hint(file);
                    stream_channel_file(file, &index, &fallback, &mut state)?;
                    if state.stopped || state.failure.is_some() {
                        break;
                    }
                }
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        Ok(())
    }
}

/// Stream one channel file: header pass (identity + the empty-export
/// refusal), then the `messages` array element by element. Read and
/// structural errors name the file, exactly as the directory collect
/// path does.
fn stream_channel_file(
    file: &Path,
    index: &HashMap<String, String>,
    chat_id_fallback: &str,
    state: &mut crate::bounded::ConsumerState<'_>,
) -> Result<()> {
    let header: DiscordHeader = serde_json::from_reader(BufReader::new(
        fs::File::open(file).map_err(|e| read_failed(file, e))?,
    ))
    .map_err(|e| channel_file_failed(file, e.into()))?;
    // The same refusal as the whole-file parser: an object with none of
    // the three Discord members is not a channel file (a stray
    // `notes.json` in the directory).
    if header.messages.is_empty() && header.channel.is_none() && header.guild.is_none() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{}: not a Discord channel export: no messages, channel, or guild member",
                file.display()
            ),
        )));
    }
    let ctx = ChannelContext::from_header(&header, index, chat_id_fallback);
    let result = {
        let visitor = MessagesVisitor { ctx: &ctx, state };
        let mut de = serde_json::Deserializer::from_reader(BufReader::new(
            fs::File::open(file).map_err(|e| read_failed(file, e))?,
        ));
        de.deserialize_map(visitor)
    };
    if let Some(e) = state.failure.take() {
        return Err(e);
    }
    match result {
        Ok(()) => Ok(()),
        Err(e) if state.stopped && crate::bounded::ConsumerState::is_stop_sentinel(&e) => Ok(()),
        Err(e) => Err(Error::Io(channel_file_failed(file, e.into()))),
    }
}

/// Parse every channel file of a Discord messages directory (or a
/// package root: its `messages/` subdirectory wins when present).
fn import_directory(root: &Path) -> Result<Collected> {
    let files = channel_files(root)?;
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "no Discord channel .json files found under {} \
                 (expected the package's messages/ directory or its parent)",
                root.display()
            ),
        )
        .into());
    }
    let index = index_map(root);
    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for file in &files {
        let bytes = fs::read(file).map_err(|e| read_failed(file, e))?;
        let fallback = file_stem_hint(file);
        let collected = parse_discord_channel(&bytes, &index, &fallback)
            .map_err(|e| channel_file_failed(file, e))?;
        messages.extend(collected.messages);
        failed += collected.failed;
    }
    Ok(Collected { messages, failed })
}

/// The directory holding channel files: a package root's `messages/`
/// subdirectory when present, the path itself otherwise.
fn messages_dir(root: &Path) -> PathBuf {
    let inner = root.join("messages");
    if inner.is_dir() {
        inner
    } else {
        root.to_path_buf()
    }
}

/// Channel files of a messages directory, sorted by name: every `.json`
/// file except `index.json` (which maps ids to names but holds no
/// messages).
fn channel_files(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = messages_dir(root);
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let is_json = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        if path.file_name().is_some_and(|n| n == "index.json") {
            continue;
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

/// Channel-id → channel-path map from a sibling `index.json`; empty when
/// absent or unparseable (names then fall back to the channel ids).
fn index_map(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(bytes) = fs::read(messages_dir(root).join("index.json")) else {
        return map;
    };
    let Ok(index) = serde_json::from_slice::<Value>(&bytes) else {
        return map;
    };
    if let Some(entries) = index.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let (Some(id), Some(path)) = (
                entry.get("id").and_then(id_to_string),
                entry.get("channel_path").and_then(Value::as_str),
            ) else {
                continue;
            };
            if !path.is_empty() {
                map.insert(id, path.to_string());
            }
        }
    }
    map
}

/// The export top level; every message object is kept as raw JSON so
/// nothing in it is lost. DM files carry no `guild` key.
#[derive(Debug, Deserialize)]
struct DiscordExport {
    #[serde(default)]
    channel: Option<Value>,
    #[serde(default)]
    guild: Option<Value>,
    #[serde(default)]
    messages: Vec<Value>,
}

/// Parse one Discord channel file into messages.
///
/// `index` maps channel ids to package `channel_path` names (used when a
/// channel has no name of its own, i.e. DMs); `chat_id_fallback` (the
/// file stem) is used when the file carries no channel id at all.
/// `external_id` is `<channel id>/<message id>`: message snowflakes are
/// globally unique, but namespacing by the stable channel id keeps the
/// key readable and safe — a channel renamed between exports dedups on
/// ids, never duplicating on the new name. Entries that are not objects,
/// lack a `type`, or lack an id count as failed; system entries are
/// skipped silently.
pub fn parse_discord_channel(
    bytes: &[u8],
    index: &HashMap<String, String>,
    chat_id_fallback: &str,
) -> Result<Collected> {
    let export: DiscordExport = serde_json::from_slice(bytes)?;
    // A JSON object with none of the three Discord members is not a
    // channel file at all (a stray `notes.json` in the directory);
    // refuse instead of archiving an empty export.
    if export.messages.is_empty() && export.channel.is_none() && export.guild.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a Discord channel export: no messages, channel, or guild member",
        )
        .into());
    }
    let ctx = ChannelContext::from_parts(
        export.channel.as_ref(),
        export.guild.as_ref(),
        index,
        chat_id_fallback,
    );

    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for entry in &export.messages {
        match discord_entry(entry, &ctx) {
            EntryOutcome::Message(msg) => messages.push(msg),
            EntryOutcome::Failed { .. } => failed += 1,
            EntryOutcome::Skip => {}
        }
    }
    Ok(Collected { messages, failed })
}

/// The header view of the streaming pass: `channel` and `guild` are read
/// (small objects), `messages` is enforced to be an array (and its
/// emptiness is checkable through the zero-sized `IgnoredAny` elements'
/// count) but skipped element by element.
#[derive(Debug, Deserialize)]
struct DiscordHeader {
    #[serde(default)]
    channel: Option<Value>,
    #[serde(default)]
    guild: Option<Value>,
    messages: Vec<serde::de::IgnoredAny>,
}

/// Per-channel identity shared by every entry of one channel file: the
/// dedup namespace (stable channel id, fallback file stem) and the
/// display grouping (names, then the package index, then the id).
struct ChannelContext {
    dedup_channel: String,
    chat_id: String,
}

impl ChannelContext {
    fn from_header(
        header: &DiscordHeader,
        index: &HashMap<String, String>,
        chat_id_fallback: &str,
    ) -> Self {
        Self::from_parts(
            header.channel.as_ref(),
            header.guild.as_ref(),
            index,
            chat_id_fallback,
        )
    }

    fn from_parts(
        channel: Option<&Value>,
        guild: Option<&Value>,
        index: &HashMap<String, String>,
        chat_id_fallback: &str,
    ) -> Self {
        let (channel_id, channel_name) = channel
            .map(|c| {
                (
                    c.get("id").and_then(id_to_string).unwrap_or_default(),
                    field_str(c, "name").unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        let guild_name = guild.and_then(|g| field_str(g, "name")).unwrap_or_default();

        // Dedup follows the stable channel id (fallback: the file stem);
        // display grouping prefers names, then the package index, then ids.
        let dedup_channel = if channel_id.is_empty() {
            chat_id_fallback.to_string()
        } else {
            channel_id.clone()
        };
        let chat_id = if !guild_name.is_empty() && !channel_name.is_empty() {
            format!("{guild_name}/{channel_name}")
        } else if !channel_name.is_empty() {
            channel_name
        } else if let Some(path) = index.get(&channel_id) {
            path.clone()
        } else {
            dedup_channel.clone()
        };
        Self {
            dedup_channel,
            chat_id,
        }
    }
}

/// Convert one `messages` array entry through the shared mapping (used by
/// both the whole-file parser and the streaming pass, so their behavior
/// cannot drift apart).
fn discord_entry(entry: &Value, ctx: &ChannelContext) -> EntryOutcome {
    let Some(obj) = entry.as_object() else {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "entry is not a JSON object".to_string(),
        };
    };
    // System entries (joins, pins, calls) carry no body: skipped. An
    // entry with no type at all is malformed (real exports always type
    // them) and counts as failed.
    match obj.get("type").and_then(Value::as_str) {
        Some(t) if CONTENT_TYPES.contains(&t) => {}
        Some(_) => return EntryOutcome::Skip,
        None => {
            return EntryOutcome::Failed {
                external_ref: obj.get("id").and_then(id_to_string).unwrap_or_default(),
                reason: "message entry has no type".to_string(),
            }
        }
    }
    let Some(id) = obj.get("id").and_then(id_to_string) else {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "message entry has no id".to_string(),
        };
    };

    let author = author_name(obj);
    let sent_at = discord_sent_at(obj);
    let body = obj.get("content").and_then(Value::as_str).unwrap_or("");
    let external_id = format!("{}/{}", ctx.dedup_channel, id);

    let mut msg = NewMessage::new(DISCORD_SOURCE, external_id, author, sent_at, body);
    msg.channel = "chat".to_string();
    msg.chat_id = ctx.chat_id.clone();
    // The whole original object: reactions, attachments, embeds,
    // edited timestamps, mentions — all stay recorded in raw_json.
    msg.raw_json = entry.to_string();
    EntryOutcome::Message(msg)
}

/// serde visitor of the streaming pass: walks the channel object,
/// streaming the `messages` array element by element; every other member
/// is skipped. Stops reading as soon as the consumer stops or fails (the
/// deserializer is used directly, without an end-of-input check).
struct MessagesVisitor<'a, 'c> {
    ctx: &'a ChannelContext,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::Visitor<'de> for MessagesVisitor<'a, 'c> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Discord channel export object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "messages" {
                map.next_value_seed(MessagesSeed {
                    ctx: self.ctx,
                    state: self.state,
                })?;
                // Messages streamed; trailing members are irrelevant.
                return Ok(());
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
    ctx: &'a ChannelContext,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::DeserializeSeed<'de> for MessagesSeed<'a, 'c> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
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
            if !self.state.offer(discord_entry(&entry, self.ctx)) {
                return Err(crate::bounded::ConsumerState::stop());
            }
        }
        Ok(())
    }
}

/// True when the bytes are a Discord channel file: JSON carrying a
/// `messages` array plus a `channel` (or `guild`) object — the shape that
/// distinguishes it from a Telegram `result.json`, which also has a
/// top-level `messages` array.
pub fn is_discord_export(bytes: &[u8]) -> bool {
    is_discord_export_shape(bytes)
}

/// [`is_discord_export`] streamed from a reader: only the top-level
/// members are examined (the `messages` array is skipped element by
/// element through zero-sized `IgnoredAny`s), so detection of an
/// arbitrarily large channel file stays bounded.
pub fn is_discord_export_reader<R: std::io::Read>(reader: R) -> bool {
    struct Detect {
        has_messages_array: bool,
        has_channel_or_guild: bool,
    }
    impl<'de> serde::de::Visitor<'de> for Detect {
        type Value = bool;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a Discord channel export object")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<bool, A::Error> {
            let mut d = Detect {
                has_messages_array: false,
                has_channel_or_guild: false,
            };
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "messages" => {
                        // An array member at all — even empty — is the
                        // Telegram/Discord distinguishing shape.
                        let messages: Vec<serde::de::IgnoredAny> = map.next_value()?;
                        d.has_messages_array = true;
                        let _ = messages.len();
                    }
                    "channel" | "guild" => {
                        let v: Value = map.next_value()?;
                        if v.is_object() {
                            d.has_channel_or_guild = true;
                        }
                    }
                    _ => {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
            }
            Ok(d.has_messages_array && d.has_channel_or_guild)
        }
    }
    serde_json::Deserializer::from_reader(BufReader::new(reader))
        .deserialize_map(Detect {
            has_messages_array: false,
            has_channel_or_guild: false,
        })
        .unwrap_or(false)
}

/// The object-shape half of [`is_discord_export`] (bytes variant).
fn is_discord_export_shape(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let Some(obj) = v.as_object() else {
        return false;
    };
    if !obj.get("messages").is_some_and(Value::is_array) {
        return false;
    }
    obj.get("channel").is_some_and(Value::is_object)
        || obj.get("guild").is_some_and(Value::is_object)
}

/// True when `root` looks importable as Discord: it resolves to a
/// messages directory in which any channel file parses as a Discord
/// export. A corrupt file that sorts first must not hide a genuine
/// package; the import itself still names unreadable files.
pub fn is_discord_export_dir(root: &Path) -> bool {
    let Ok(files) = channel_files(root) else {
        return false;
    };
    files
        .iter()
        .filter_map(|f| fs::read(f).ok())
        .any(|bytes| is_discord_export(&bytes))
}

/// Author display name: the server nickname wins, then the global
/// username, then the raw author id; empty when there is no author
/// object at all.
fn author_name(obj: &serde_json::Map<String, Value>) -> String {
    obj.get("author")
        .and_then(Value::as_object)
        .map(|a| {
            str_field(a.get("nickname"))
                .or_else(|| str_field(a.get("name")))
                .or_else(|| a.get("id").and_then(id_to_string))
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Timestamp: the package's `YYYY-MM-DD HH:MM:SS.mmm+00:00` form is
/// UTC-correct; compact-offset and RFC 3339 spellings are accepted as
/// fallbacks. Milliseconds are dropped (the archive stores whole
/// seconds). Empty when nothing parses.
fn discord_sent_at(obj: &serde_json::Map<String, Value>) -> String {
    let Some(s) = obj.get("timestamp").and_then(Value::as_str) else {
        return String::new();
    };
    const RFC3339: &str = "%Y-%m-%dT%H:%M:%SZ";
    let parsed = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z")
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%z"))
        .or_else(|_| DateTime::parse_from_rfc3339(s));
    parsed
        .map(|t| {
            let utc: DateTime<Utc> = t.into();
            utc.format(RFC3339).to_string()
        })
        .unwrap_or_default()
}

/// A non-empty string member of an object.
fn field_str(obj: &Value, key: &str) -> Option<String> {
    str_field(obj.get(key))
}

/// A non-empty string value.
fn str_field(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Snowflake ids are quoted strings in the package, but tolerate numbers
/// from other tooling: either form maps to its string spelling.
/// Whitespace-only strings are not ids and count as absent.
fn id_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => (!s.trim().is_empty()).then(|| s.clone()),
        Value::Number(n) => n
            .as_u64()
            .map(|u| u.to_string())
            .or_else(|| n.as_i64().map(|i| i.to_string())),
        _ => None,
    }
}

/// An unreadable or unparseable channel file aborts the directory import;
/// the error names the file so the bad one can be found in a large package.
fn read_failed(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

fn channel_file_failed(path: &Path, e: crate::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {e}", path.display()),
    )
}

/// Import-hint chat key when the file carries no channel id: its stem.
fn file_stem_hint(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
