//! Archive statistics (the dashboard data path): totals, date range,
//! per-source / per-day / per-chat / per-author aggregates, tie-break and
//! limit behavior, and the exclusions (unknown `sent_at`, empty `chat_id`).

use vault_core::message::NewMessage;
use vault_core::stats::{ArchiveStats, AuthorCount, ChatCount, DayCount, MAX_TOP};
use vault_core::storage::Storage;

fn storage() -> Storage {
    Storage::open_in_memory().expect("open in-memory storage")
}

/// Insert a fully-specified message (the `NewMessage::new` helper hardcodes
/// `channel: "email"` and `chat_id: ""`, which would skew chat grouping).
fn put(s: &Storage, source: &str, id: &str, author: &str, sent_at: &str, chat_id: &str) {
    s.insert_message(&NewMessage {
        source: source.into(),
        external_id: id.into(),
        channel: "chat".into(),
        chat_id: chat_id.into(),
        author: author.into(),
        sent_at: sent_at.into(),
        body: format!("body of {id}"),
        raw_json: "{}".into(),
        conversation_key: String::new(),
        reply_refs: Vec::new(),
        original_timestamp: String::new(),
        timezone: String::new(),
        time_confidence: String::new(),
        attachments: Vec::new(),
    })
    .unwrap();
}

#[test]
fn empty_archive_reports_zeroes() {
    let s = storage();
    let stats = s.archive_stats(10).unwrap();
    assert_eq!(
        stats,
        ArchiveStats {
            total: 0,
            first_sent_at: None,
            last_sent_at: None,
            by_source: Vec::new(),
            by_day: Vec::new(),
            top_chats: Vec::new(),
            top_authors: Vec::new(),
        }
    );
}

#[test]
fn aggregates_by_source_author_chat_and_day() {
    let s = storage();
    // source "wa", chat "alpha": ana ×3, marko ×1, across two days.
    put(&s, "wa", "a1", "ana", "2026-01-01T09:00:00Z", "alpha");
    put(&s, "wa", "a2", "ana", "2026-01-01T10:00:00Z", "alpha");
    put(&s, "wa", "a3", "marko", "2026-01-02T11:00:00Z", "alpha");
    put(&s, "wa", "a4", "ana", "2026-01-02T12:00:00Z", "alpha");
    // source "mbox", no chat: two mails, one dated, one not.
    put(&s, "mbox", "m1", "bot", "2025-12-31T23:00:00Z", "");
    put(&s, "mbox", "m2", "bot", "", "");
    // source "tg", chat "beta": one message on the first day.
    put(&s, "tg", "t1", "ana", "2026-01-01T08:00:00Z", "beta");

    let stats = s.archive_stats(10).unwrap();

    assert_eq!(stats.total, 7);
    assert_eq!(stats.first_sent_at.as_deref(), Some("2025-12-31T23:00:00Z"));
    assert_eq!(stats.last_sent_at.as_deref(), Some("2026-01-02T12:00:00Z"));

    // by_source: count descending, then source name for ties.
    assert_eq!(
        stats
            .by_source
            .iter()
            .map(|s| (s.source.as_str(), s.count))
            .collect::<Vec<_>>(),
        vec![("wa", 4), ("mbox", 2), ("tg", 1)]
    );

    // by_day: ascending, unknown sent_at excluded, empty days simply absent.
    assert_eq!(
        stats.by_day,
        vec![
            DayCount {
                day: "2025-12-31".into(),
                count: 1
            },
            DayCount {
                day: "2026-01-01".into(),
                count: 3
            },
            DayCount {
                day: "2026-01-02".into(),
                count: 2
            },
        ]
    );

    // top_chats: count descending, then (source, chat_id); empty chat_id
    // (the two mbox mails) is not a conversation and never appears.
    assert_eq!(
        stats.top_chats,
        vec![
            ChatCount {
                source: "wa".into(),
                chat_id: "alpha".into(),
                count: 4
            },
            ChatCount {
                source: "tg".into(),
                chat_id: "beta".into(),
                count: 1
            },
        ]
    );

    // top_authors: ana 4, bot 2 (the undated mail still counts), marko 1.
    assert_eq!(
        stats.top_authors,
        vec![
            AuthorCount {
                author: "ana".into(),
                count: 4
            },
            AuthorCount {
                author: "bot".into(),
                count: 2
            },
            AuthorCount {
                author: "marko".into(),
                count: 1
            },
        ]
    );
}

#[test]
fn unknown_sent_at_only_still_reports_total_and_groups() {
    let s = storage();
    put(&s, "wa", "u1", "ana", "", "alpha");
    let stats = s.archive_stats(10).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.first_sent_at, None);
    assert_eq!(stats.last_sent_at, None);
    assert!(stats.by_day.is_empty());
    // The chat and author still aggregate.
    assert_eq!(stats.top_chats[0].count, 1);
    assert_eq!(stats.top_authors[0].author, "ana");
}

#[test]
fn ties_break_deterministically_by_name() {
    let s = storage();
    // Two chats and two authors, all with equal counts.
    put(&s, "wa", "1", "zed", "2026-01-01T00:00:00Z", "b-chat");
    put(&s, "wa", "2", "ana", "2026-01-01T01:00:00Z", "a-chat");
    let stats = s.archive_stats(10).unwrap();
    assert_eq!(
        stats
            .top_chats
            .iter()
            .map(|c| (c.source.as_str(), c.chat_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("wa", "a-chat"), ("wa", "b-chat")]
    );
    assert_eq!(
        stats
            .top_authors
            .iter()
            .map(|a| a.author.as_str())
            .collect::<Vec<_>>(),
        vec!["ana", "zed"]
    );
}

#[test]
fn top_limit_clamps_and_bounds() {
    let s = storage();
    for i in 0..12 {
        put(
            &s,
            "wa",
            &format!("m{i}"),
            &format!("author-{i:02}"),
            "2026-01-01T00:00:00Z",
            "chat",
        );
    }
    // 0 is not a useful table size; the default minimum is 1.
    assert_eq!(s.archive_stats(0).unwrap().top_authors.len(), 1);
    assert_eq!(s.archive_stats(3).unwrap().top_authors.len(), 3);
    assert_eq!(s.archive_stats(12).unwrap().top_authors.len(), 12);
    // Absurdly large tops clamp to MAX_TOP, never to the row count.
    assert_eq!(s.archive_stats(u32::MAX).unwrap().top_authors.len(), 12);
    assert_eq!(MAX_TOP, 100);
    // Chats respect the same bound.
    assert_eq!(s.archive_stats(1).unwrap().top_chats.len(), 1);
}

#[test]
fn dashboard_agrees_with_plain_stats() {
    let s = storage();
    put(&s, "wa", "1", "ana", "2026-01-01T00:00:00Z", "alpha");
    put(&s, "wa", "2", "ana", "2026-01-02T00:00:00Z", "alpha");
    put(&s, "mbox", "3", "bot", "", "");
    let plain = s.stats().unwrap();
    let dash = s.archive_stats(10).unwrap();
    assert_eq!(plain.total, dash.total);
    assert_eq!(plain.by_source, dash.by_source);
}

#[test]
fn same_day_counts_across_months_and_years_order_correctly() {
    let s = storage();
    put(&s, "wa", "1", "ana", "2024-12-31T23:59:59Z", "c");
    put(&s, "wa", "2", "ana", "2025-01-01T00:00:00Z", "c");
    put(&s, "wa", "3", "ana", "2026-01-01T00:00:00Z", "c");
    let days: Vec<String> = s
        .archive_stats(10)
        .unwrap()
        .by_day
        .into_iter()
        .map(|d| d.day)
        .collect();
    assert_eq!(days, ["2024-12-31", "2025-01-01", "2026-01-01"]);
}

/// The counter tables are maintained by triggers on EVERY write path, not
/// just `insert_message`: raw INSERT, UPDATE, and DELETE against the
/// canonical archive_messages table must keep the invariant that every
/// counter sums to COUNT(*) and that the aggregates match a fresh GROUP
/// BY. The product itself never updates or deletes archive rows, but the
/// invariant must hold under any writer.
#[test]
fn counters_track_raw_inserts_updates_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("counters.db");
    let s = Storage::open(&db).unwrap();
    put(&s, "wa", "1", "ana", "2026-01-01T00:00:00Z", "alpha");
    put(&s, "wa", "2", "marko", "2026-01-02T00:00:00Z", "alpha");
    put(&s, "mbox", "3", "ana", "", "");

    // A second connection on the same file performs raw writes; the
    // triggers fire for it exactly as for the API path.
    {
        let raw = rusqlite::Connection::open(&db).unwrap();
        raw.execute(
            "INSERT INTO source_instances (id, legacy_namespace, kind, label)
             VALUES ('si-tg', 'tg', 'legacy', 'tg')",
            [],
        )
        .unwrap();
        raw.execute(
            "INSERT INTO archive_messages (source_instance_id, external_id, channel, chat_id, author, sent_instant, original_timestamp, time_confidence, body)
             VALUES ('si-tg', 'raw-1', 'chat', 'beta', 'jelena', '2026-01-03T00:00:00Z', '2026-01-03T00:00:00Z', 'legacy_assumed', 'raw insert')",
            [],
        )
        .unwrap();
        // Move a message to a different chat, author, and day.
        raw.execute(
            "UPDATE archive_messages SET chat_id = 'beta', author = 'ana', sent_instant = '2026-01-04T00:00:00Z'
             WHERE external_id = '2'",
            [],
        )
        .unwrap();
        // And delete one entirely.
        raw.execute("DELETE FROM archive_messages WHERE external_id = '3'", [])
            .unwrap();
    }

    let dash = s.archive_stats(10).unwrap();
    assert_eq!(dash.total, 3);
    let check = |label: &str, counted: u64, expected: u64| {
        assert_eq!(counted, expected, "{label} counter drifted");
    };
    check("total", dash.total, 3);
    check(
        "by_source sum",
        dash.by_source.iter().map(|x| x.count).sum(),
        3,
    );
    check(
        "by_day sum (dated)",
        dash.by_day.iter().map(|x| x.count).sum(),
        3,
    );
    check(
        "top_chats",
        dash.top_chats
            .iter()
            .find(|c| c.source == "wa" && c.chat_id == "alpha")
            .map(|c| c.count)
            .unwrap_or(0),
        1,
    );
    // author counts after the update: ana 2 (was 1, gained the updated
    // row, which moved from marko), marko 0 rows, jelena 1.
    let ana = dash
        .top_authors
        .iter()
        .find(|a| a.author == "ana")
        .map(|a| a.count)
        .unwrap_or(0);
    let marko = dash
        .top_authors
        .iter()
        .find(|a| a.author == "marko")
        .map(|a| a.count)
        .unwrap_or(0);
    assert_eq!(
        (ana, marko),
        (2, 0),
        "author counters must follow the update"
    );

    // Cross-check against live GROUP BYs over the raw table.
    let raw = rusqlite::Connection::open(&db).unwrap();
    let live_total: i64 = raw
        .query_row("SELECT COUNT(*) FROM archive_messages", [], |r| r.get(0))
        .unwrap();
    let counter_total: i64 = raw
        .query_row("SELECT SUM(n) FROM stats_source", [], |r| r.get(0))
        .unwrap();
    assert_eq!(live_total, counter_total);
    let live_day: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE sent_instant != ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let counter_day: i64 = raw
        .query_row("SELECT SUM(n) FROM stats_day WHERE day != ''", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(live_day, counter_day);
    let live_chat: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE chat_id != ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let counter_chat: i64 = raw
        .query_row(
            "SELECT SUM(n) FROM stats_chat WHERE chat_id != ''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live_chat, counter_chat);
}

/// Deleting every message of a group must remove its counter rows (or at
/// least zero them): an emptied archive reports as empty.
#[test]
fn counters_shrink_back_to_zero_when_messages_are_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("shrink.db");
    let s = Storage::open(&db).unwrap();
    put(&s, "wa", "1", "ana", "2026-01-01T00:00:00Z", "alpha");
    put(&s, "wa", "2", "ana", "2026-01-02T00:00:00Z", "alpha");
    {
        let raw = rusqlite::Connection::open(&db).unwrap();
        raw.execute("DELETE FROM archive_messages", []).unwrap();
    }
    let dash = s.archive_stats(10).unwrap();
    assert_eq!(dash.total, 0);
    assert_eq!(dash.by_source, Vec::new());
    assert!(dash.by_day.is_empty());
    assert!(dash.top_chats.is_empty());
    assert!(dash.top_authors.is_empty());
    assert_eq!(dash.first_sent_at, None);
    assert_eq!(dash.last_sent_at, None);
}
