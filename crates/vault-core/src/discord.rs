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
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::Result;

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
    let (channel_id, channel_name) = export
        .channel
        .as_ref()
        .map(|c| {
            (
                c.get("id").and_then(id_to_string).unwrap_or_default(),
                field_str(c, "name").unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let guild_name = export
        .guild
        .as_ref()
        .and_then(|g| field_str(g, "name"))
        .unwrap_or_default();

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

    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for entry in &export.messages {
        let Some(obj) = entry.as_object() else {
            failed += 1;
            continue;
        };
        // System entries (joins, pins, calls) carry no body: skipped. An
        // entry with no type at all is malformed (real exports always
        // type them) and counts as failed.
        match obj.get("type").and_then(Value::as_str) {
            Some(t) if CONTENT_TYPES.contains(&t) => {}
            Some(_) => continue,
            None => {
                failed += 1;
                continue;
            }
        }
        let Some(id) = obj.get("id").and_then(id_to_string) else {
            failed += 1;
            continue;
        };

        let author = author_name(obj);
        let sent_at = discord_sent_at(obj);
        let body = obj.get("content").and_then(Value::as_str).unwrap_or("");
        let external_id = format!("{}/{}", dedup_channel, id);

        let mut msg = NewMessage::new(DISCORD_SOURCE, external_id, author, sent_at, body);
        msg.channel = "chat".to_string();
        msg.chat_id = chat_id.clone();
        // The whole original object: reactions, attachments, embeds,
        // edited timestamps, mentions — all stay recorded in raw_json.
        msg.raw_json = entry.to_string();
        messages.push(msg);
    }
    Ok(Collected { messages, failed })
}

/// True when the bytes are a Discord channel file: JSON carrying a
/// `messages` array plus a `channel` (or `guild`) object — the shape that
/// distinguishes it from a Telegram `result.json`, which also has a
/// top-level `messages` array.
pub fn is_discord_export(bytes: &[u8]) -> bool {
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
/// messages directory whose first channel file parses as a Discord
/// export.
pub fn is_discord_export_dir(root: &Path) -> bool {
    let Ok(files) = channel_files(root) else {
        return false;
    };
    files
        .first()
        .and_then(|f| fs::read(f).ok())
        .is_some_and(|bytes| is_discord_export(&bytes))
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
