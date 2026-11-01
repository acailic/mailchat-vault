use std::collections::HashMap;

use vault_core::discord::{is_discord_export, DiscordSource, DISCORD_SOURCE};
use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

// Realistic Discord Data Package guild-channel file: string snowflake
// ids, millisecond timestamps with +00:00 offset, an author with both a
// global name and a server nickname, a reply, an attachment-only message
// with reactions, an edited message, a bot, system entries, and three
// broken entries.
const GUILD_CHANNEL_JSON: &str = r##"{
  "channel": { "id": "900000000000000101", "name": "general" },
  "guild": { "id": "800000000000000001", "name": "Ekipa HQ" },
  "messages": [
    {
      "id": "1700000000000000001",
      "type": "Default",
      "timestamp": "2026-10-31 18:00:05.123+00:00",
      "timestamp_edited": null,
      "is_pinned": false,
      "content": "Zdravo ekipa! Arhiva je gotova 🎉",
      "attachments": [],
      "embeds": [],
      "mentions": [],
      "author": {
        "id": "111000000000000001",
        "name": "marko",
        "discriminator": "0001",
        "nickname": "Marko Horvat",
        "color": "#faa61a",
        "isBot": false,
        "avatarUrl": "https://cdn.discordapp.com/avatars/111000000000000001/a.png?size=32"
      }
    },
    {
      "id": "1700000000000000002",
      "type": "Reply",
      "timestamp": "2026-10-31 18:00:41.999+00:00",
      "timestamp_edited": "2026-10-31 18:01:02.000+00:00",
      "is_pinned": false,
      "content": "odgovor na arhivu",
      "attachments": [],
      "embeds": [],
      "mentions": [{ "id": "111000000000000001", "name": "marko" }],
      "author": {
        "id": "111000000000000002",
        "name": "jelena",
        "nickname": "Jelena Babić"
      }
    },
    {
      "id": "1700000000000000003",
      "type": "Default",
      "timestamp": "2026-10-31 18:02:00.000+00:00",
      "content": "",
      "attachments": [
        {
          "id": "1160000000000000003",
          "url": "https://cdn.discordapp.com/attachments/900000000000000101/1700000000000000003/plan.txt",
          "filename": "plan.txt",
          "size": 4096
        }
      ],
      "embeds": [],
      "reactions": [
        { "emoji": { "name": "🔥", "id": null }, "count": 2, "me": false },
        { "emoji": { "name": "eyes", "id": "9876543210987654321" }, "count": 1, "me": true }
      ],
      "author": { "id": "111000000000000001", "name": "marko" }
    },
    {
      "id": "1700000000000000004",
      "type": "Default",
      "timestamp": "2026-10-31 18:03:00+00:00",
      "content": "bot na poslu",
      "author": { "id": "111000000000000009", "name": "vaultbot", "isBot": true }
    },
    { "id": "1700000000000000005", "type": "Default", "timestamp": "2026-10-31 18:04:00+00:00", "content": "bez autora" },
    { "id": "1700000000000000006", "type": "GuildMemberJoin", "timestamp": "2026-10-31 18:05:00+00:00", "content": "" },
    { "id": "1700000000000000007", "type": "ChannelPinnedMessage", "timestamp": "2026-10-31 18:06:00+00:00", "content": "" },
    { "type": "Default", "timestamp": "2026-10-31 18:07:00+00:00", "content": "nema id polja" },
    { "id": "1700000000000000008", "timestamp": "2026-10-31 18:08:00+00:00", "content": "nema type polja" },
    "nije objekt"
  ]
}"##;

#[test]
fn parses_fields_from_guild_channel_json() {
    let collected = vault_core::discord::parse_discord_channel(
        GUILD_CHANNEL_JSON.as_bytes(),
        &HashMap::new(),
        "fallback",
    )
    .unwrap();
    // 5 real messages (Default + Reply); GuildMemberJoin and
    // ChannelPinnedMessage are system entries (skipped, not counted as
    // seen); one entry lacks an id, one lacks a type, and one is not an
    // object (all failed).
    assert_eq!(collected.messages.len(), 5);
    assert_eq!(collected.failed, 3);

    let first = &collected.messages[0];
    assert_eq!(first.source, DISCORD_SOURCE);
    assert_eq!(first.channel, "chat");
    assert_eq!(first.chat_id, "Ekipa HQ/general");
    assert_eq!(first.external_id, "900000000000000101/1700000000000000001");
    // The server nickname wins over the global username.
    assert_eq!(first.author, "Marko Horvat");
    // Milliseconds are dropped; the +00:00 offset becomes UTC Z form.
    assert_eq!(first.sent_at, "2026-10-31T18:00:05Z");
    assert!(first.body.contains("Arhiva je gotova"));

    // Replies are real messages; edits are only recorded in raw_json.
    let reply = &collected.messages[1];
    assert_eq!(reply.body, "odgovor na arhivu");
    assert_eq!(reply.author, "Jelena Babić");
    assert_eq!(reply.sent_at, "2026-10-31T18:00:41Z");
    let raw: serde_json::Value = serde_json::from_str(&reply.raw_json).unwrap();
    assert_eq!(
        raw["timestamp_edited"].as_str().unwrap(),
        "2026-10-31 18:01:02.000+00:00"
    );
    assert_eq!(raw["mentions"][0]["name"], "marko");

    // Attachment-only message: empty body, but reactions and attachment
    // metadata survive verbatim in raw_json.
    let media = &collected.messages[2];
    assert_eq!(media.body, "");
    let raw: serde_json::Value = serde_json::from_str(&media.raw_json).unwrap();
    assert_eq!(raw["attachments"][0]["filename"], "plan.txt");
    assert_eq!(raw["attachments"][0]["size"], 4096);
    assert_eq!(raw["reactions"][0]["emoji"]["name"], "🔥");
    assert_eq!(raw["reactions"][0]["count"], 2);

    // Without a nickname the global username is the author; a timestamp
    // without milliseconds parses the same.
    let bot = &collected.messages[3];
    assert_eq!(bot.author, "vaultbot");
    assert_eq!(bot.sent_at, "2026-10-31T18:03:00Z");

    // No author object at all: empty author, message still stored.
    assert_eq!(collected.messages[4].author, "");
    assert_eq!(collected.messages[4].body, "bez autora");
}

#[test]
fn chat_id_uses_channel_name_without_guild() {
    let dm_group = r#"{
      "channel": { "id": "411116245892313088", "name": "vikend ekipa" },
      "messages": [
        { "id": "1", "type": "Default", "timestamp": "2026-10-31 18:00:00+00:00", "content": "x" }
      ]
    }"#;
    let collected = vault_core::discord::parse_discord_channel(
        dm_group.as_bytes(),
        &HashMap::new(),
        "fallback",
    )
    .unwrap();
    assert_eq!(collected.messages[0].chat_id, "vikend ekipa");
    // The dedup namespace still comes from the channel id.
    assert_eq!(collected.messages[0].external_id, "411116245892313088/1");
}

#[test]
fn dm_channel_resolves_name_from_index() {
    // DM files carry only a channel id; the package's index.json maps it
    // to a readable path.
    let dm = r#"{
      "channel": { "id": "411116245892313088" },
      "messages": [
        { "id": "1700000000000000010", "type": "Default", "timestamp": "2026-10-31 12:00:00.500+00:00", "content": "hej", "author": { "id": "5", "name": "ana" } }
      ]
    }"#;
    let mut index = HashMap::new();
    index.insert(
        "411116245892313088".to_string(),
        "Direct Messages/@ana".to_string(),
    );
    let collected =
        vault_core::discord::parse_discord_channel(dm.as_bytes(), &index, "fallback").unwrap();
    assert_eq!(collected.messages[0].chat_id, "Direct Messages/@ana");

    // Without an index entry the channel id itself groups the DM.
    let collected =
        vault_core::discord::parse_discord_channel(dm.as_bytes(), &HashMap::new(), "fallback")
            .unwrap();
    assert_eq!(collected.messages[0].chat_id, "411116245892313088");
}

#[test]
fn timestamp_fallbacks_and_failures() {
    let variants = r#"{
      "channel": { "id": "c1", "name": "general" },
      "messages": [
        { "id": "1", "type": "Default", "timestamp": "2026-10-31 18:00:00.123456+02:00", "content": "offset shift" },
        { "id": "2", "type": "Default", "timestamp": "2026-10-31T18:00:00Z", "content": "rfc3339" },
        { "id": "3", "type": "Default", "timestamp": "2026-10-31 18:00:00+0000", "content": "compact offset" },
        { "id": "4", "type": "Default", "timestamp": "not a date", "content": "garbage" },
        { "id": "5", "type": "Default", "timestamp": 42, "content": "number stamp" },
        { "id": "6", "type": "Default", "content": "no timestamp at all" }
      ]
    }"#;
    let collected = vault_core::discord::parse_discord_channel(
        variants.as_bytes(),
        &HashMap::new(),
        "fallback",
    )
    .unwrap();
    let sent: Vec<&str> = collected
        .messages
        .iter()
        .map(|m| m.sent_at.as_str())
        .collect();
    assert_eq!(
        sent,
        [
            "2026-10-31T16:00:00Z", // +02:00 shifted to UTC
            "2026-10-31T18:00:00Z", // T separator with Z
            "2026-10-31T18:00:00Z", // compact +0000 offset
            "",                     // garbage
            "",                     // non-string
            ""                      // missing
        ]
    );
    // Unparseable timestamps never fail the message; the body stays.
    assert_eq!(collected.messages[3].body, "garbage");
    assert_eq!(collected.failed, 0);
}

#[test]
fn id_variants_and_numeric_ids() {
    let variants = r#"{
      "channel": { "id": "c1", "name": "general" },
      "messages": [
        { "id": "1700000000000000001", "type": "Default", "content": "string snowflake" },
        { "id": 1234567890123456789, "type": "Default", "content": "numeric id" },
        { "id": "  ", "type": "Default", "content": "blank id" },
        { "id": 1.5, "type": "Default", "content": "float id" },
        { "id": null, "type": "Default", "content": "null id" }
      ]
    }"#;
    let collected = vault_core::discord::parse_discord_channel(
        variants.as_bytes(),
        &HashMap::new(),
        "fallback",
    )
    .unwrap();
    // Quoted strings (the package spelling) and whole numbers both work;
    // blank (whitespace-only), float, and null ids count as failed.
    assert_eq!(collected.messages.len(), 2);
    assert_eq!(collected.failed, 3);
    assert_eq!(collected.messages[0].external_id, "c1/1700000000000000001");
    assert_eq!(collected.messages[1].external_id, "c1/1234567890123456789");
}

#[test]
fn same_message_id_in_different_channels_is_not_a_duplicate() {
    let storage = Storage::open_in_memory().unwrap();
    for (channel_id, channel_name) in [
        ("900000000000000101", "general"),
        ("900000000000000202", "random"),
    ] {
        let file = format!(
            r#"{{"channel": {{"id": "{channel_id}", "name": "{channel_name}"}}, "guild": {{"id": "800000000000000001", "name": "Ekipa HQ"}},
            "messages": [{{"id": "1700000000000000001", "type": "Default", "timestamp": "2026-10-31 18:00:00+00:00", "content": "iz {channel_name}"}}]}}"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.json");
        std::fs::write(&path, file).unwrap();
        let summary = ingest(&storage, &DiscordSource::new(&path)).unwrap();
        assert_eq!(summary.inserted, 1);
    }
    assert_eq!(storage.stats().unwrap().total, 2);
}

#[test]
fn channel_rename_between_exports_dedups_instead_of_duplicating() {
    // Same channel and message ids, renamed channel: the dedup key
    // follows the ids, not the names.
    let before = r#"{
      "channel": { "id": "900000000000000101", "name": "general" },
      "guild": { "id": "800000000000000001", "name": "Ekipa HQ" },
      "messages": [{"id": "1700000000000000001", "type": "Default", "timestamp": "2026-10-31 18:00:00+00:00", "content": "poruka"}]
    }"#;
    let after = r#"{
      "channel": { "id": "900000000000000101", "name": "općenito" },
      "guild": { "id": "800000000000000001", "name": "Ekipa HQ" },
      "messages": [{"id": "1700000000000000001", "type": "Default", "timestamp": "2026-10-31 18:00:00+00:00", "content": "poruka"}]
    }"#;
    let storage = Storage::open_in_memory().unwrap();
    for (export, expected) in [(before, (1, 0)), (after, (0, 1))] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.json");
        std::fs::write(&path, export).unwrap();
        let summary = ingest(&storage, &DiscordSource::new(&path)).unwrap();
        assert_eq!(summary.inserted, expected.0);
        assert_eq!(summary.skipped, expected.1);
    }
    assert_eq!(storage.stats().unwrap().total, 1);
}

#[test]
fn ingest_from_file_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("900000000000000101.json");
    std::fs::write(&path, GUILD_CHANNEL_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    let source = DiscordSource::new(&path);
    assert_eq!(source.name(), DISCORD_SOURCE);

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: DISCORD_SOURCE.into(),
            seen: 8,
            inserted: 5,
            skipped: 0,
            failed: 3
        }
    );
    assert_eq!(storage.stats().unwrap().total, 5);

    let again = ingest(&storage, &source).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 5);
    assert_eq!(storage.stats().unwrap().total, 5);
}

#[test]
fn imported_chat_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("900000000000000101.json");
    std::fs::write(&path, GUILD_CHANNEL_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &DiscordSource::new(&path)).unwrap();

    for term in ["arhiva", "odgovor", "jelena", "marko"] {
        let hits = storage
            .search_filtered(&build_match_query(term), None, None, 10)
            .unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
    // System entries are not searchable (they were never stored).
    for term in ["pinned", "joined"] {
        assert!(storage
            .search_filtered(&build_match_query(term), None, None, 10)
            .unwrap()
            .is_empty());
    }
}

/// Writes a synthetic Discord package: `channels` channel files under
/// `messages/`, each with `per_channel` generated messages, plus the
/// id→path `index.json` and an unrelated file that must be ignored.
/// Returns the package root and the total message count.
fn write_package(root: &std::path::Path, channels: usize, per_channel: usize) -> u64 {
    let messages_dir = root.join("messages");
    std::fs::create_dir_all(&messages_dir).unwrap();
    let mut index_entries = Vec::new();
    let mut total: u64 = 0;
    for c in 0..channels {
        let channel_id = format!("90000000000000{:04}", c);
        let mut messages = Vec::new();
        for m in 0..per_channel {
            total += 1;
            messages.push(serde_json::json!({
                "id": format!("1700000000000{:08}{:04}", c, m),
                "type": "Default",
                "timestamp": format!("2026-10-{:02} {:02}:{:02}:{:02}.000+00:00", (m % 28) + 1, (m / 60) % 24, m % 60, 0),
                "content": format!("poruka {c}/{m} iz opće sobe"),
                "author": { "id": format!("11100000000000{:04}", m % 7), "name": format!("user{}", m % 7), "nickname": format!("Član {}", m % 7) }
            }));
        }
        let file = serde_json::json!({
            "channel": { "id": channel_id, "name": format!("kanal-{c}") },
            "guild": { "id": "800000000000000001", "name": "Ekipa HQ" },
            "messages": messages
        });
        std::fs::write(
            messages_dir.join(format!("{channel_id}.json")),
            file.to_string(),
        )
        .unwrap();
        index_entries.push(serde_json::json!({
            "id": channel_id,
            "channel_path": format!("Servers/Ekipa HQ/# kanal-{c}"),
            "scanned": true
        }));
    }
    // A DM channel with no name: its display name comes from the index.
    std::fs::write(
        messages_dir.join("411116245892313088.json"),
        serde_json::json!({
            "channel": { "id": "411116245892313088" },
            "messages": [{
                "id": "17000000000009999999",
                "type": "Default",
                "timestamp": "2026-10-31 09:00:00.000+00:00",
                "content": "needle-zzz-direct-message",
                "author": { "id": "5", "name": "ana" }
            }]
        })
        .to_string(),
    )
    .unwrap();
    index_entries.push(serde_json::json!({
        "id": "411116245892313088",
        "channel_path": "Direct Messages/@ana",
        "scanned": true
    }));
    std::fs::write(
        messages_dir.join("index.json"),
        serde_json::json!({ "entries": index_entries }).to_string(),
    )
    .unwrap();
    // Non-JSON and non-channel files must be ignored.
    std::fs::write(messages_dir.join("README.txt"), "not a channel").unwrap();
    total + 1 // the DM needle message
}

#[test]
fn directory_import_walks_all_channel_files() {
    let root = tempfile::tempdir().unwrap();
    let total = write_package(root.path(), 3, 4);

    let storage = Storage::open_in_memory().unwrap();
    let summary = ingest(&storage, &DiscordSource::new(root.path())).unwrap();
    assert_eq!(summary.inserted, total);
    assert_eq!(summary.failed, 0);
    assert_eq!(storage.stats().unwrap().total, total);

    // The nameless DM grouped under its index path, not under an id.
    let dm_hits = storage
        .search_filtered(&build_match_query("needle-zzz"), None, None, 10)
        .unwrap();
    assert_eq!(dm_hits.len(), 1);
    assert_eq!(dm_hits[0].chat_id, "Direct Messages/@ana");

    // Re-importing the whole directory is idempotent.
    let again = ingest(&storage, &DiscordSource::new(root.path())).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, total);
    assert_eq!(storage.stats().unwrap().total, total);
}

#[test]
fn messages_directory_itself_is_importable() {
    // Picking the inner messages/ dir (not the package root) works too.
    let root = tempfile::tempdir().unwrap();
    let total = write_package(root.path(), 2, 2);

    let storage = Storage::open_in_memory().unwrap();
    let summary = ingest(&storage, &DiscordSource::new(root.path().join("messages"))).unwrap();
    assert_eq!(summary.inserted, total);
}

#[test]
fn single_dm_channel_file_uses_sibling_index() {
    // Picking one channel file straight out of a package's messages/
    // still resolves DM names through the sibling index.json.
    let root = tempfile::tempdir().unwrap();
    write_package(root.path(), 1, 1);
    let file = root.path().join("messages").join("411116245892313088.json");

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &DiscordSource::new(&file)).unwrap();
    let hits = storage
        .search_filtered(&build_match_query("needle-zzz"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chat_id, "Direct Messages/@ana");
}

#[test]
fn large_package_imports_and_reimports() {
    // Thousands of messages: 20 channels x 500 + the DM needle.
    let root = tempfile::tempdir().unwrap();
    let total = write_package(root.path(), 20, 500);
    assert!(total >= 10_000);

    let storage = Storage::open_in_memory().unwrap();
    let summary = ingest(&storage, &DiscordSource::new(root.path())).unwrap();
    assert_eq!(summary.inserted, total);
    assert_eq!(summary.failed, 0);
    assert_eq!(storage.stats().unwrap().total, total);

    let again = ingest(&storage, &DiscordSource::new(root.path())).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, total);

    // Search still finds a needle among thousands and hits every author.
    let needle = storage
        .search_filtered(&build_match_query("needle-zzz"), None, None, 10)
        .unwrap();
    assert_eq!(needle.len(), 1);
    let author = storage
        .search_filtered(&build_match_query("član 3"), None, None, 5)
        .unwrap();
    assert!(!author.is_empty());
}

#[test]
fn empty_and_foreign_directories_report_errors() {
    let empty = tempfile::tempdir().unwrap();
    let err = DiscordSource::new(empty.path()).collect().unwrap_err();
    assert!(err.to_string().contains("no Discord channel"), "{err}");

    // A dir whose .json files are not channel files is refused.
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("notes.json"), "{\"a\": 1}").unwrap();
    assert!(DiscordSource::new(foreign.path()).collect().is_err());

    // Only index.json present: no channel files.
    let index_only = tempfile::tempdir().unwrap();
    std::fs::write(
        index_only.path().join("index.json"),
        r#"{"entries": [{"id": "1", "channel_path": "Direct Messages/@x"}]}"#,
    )
    .unwrap();
    assert!(DiscordSource::new(index_only.path()).collect().is_err());

    assert!(DiscordSource::new("/nonexistent/discord")
        .collect()
        .is_err());
}

#[test]
fn broken_json_and_unreadable_file_report_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("900000000000000101.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert!(DiscordSource::new(&path).collect().is_err());

    assert!(DiscordSource::new("/nonexistent/messages.json")
        .collect()
        .is_err());
}

#[test]
fn detection_distinguishes_discord_from_telegram() {
    // Both formats carry a top-level messages array; only the
    // channel/guild object makes it Discord.
    assert!(is_discord_export(GUILD_CHANNEL_JSON.as_bytes()));
    assert!(is_discord_export(
        br#"{"channel": {"id": "1"}, "messages": []}"#
    ));
    assert!(!is_discord_export(
        br#"{"name": "Arhiva", "id": 42, "messages": [{"id": 1, "type": "message"}]}"#
    ));
    // index.json holds no messages array: never a channel file.
    assert!(!is_discord_export(
        br#"{"entries": [{"id": "1", "channel_path": "Direct Messages/@x"}]}"#
    ));
    assert!(!is_discord_export(b""));
    assert!(!is_discord_export(br#"{"messages": "not an array"}"#));
}
