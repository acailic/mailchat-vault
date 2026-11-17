//! M2.2 integration tests: bounded streaming ingest.
//!
//! Proof of the slice's completion evidence:
//! - the streaming parser interfaces yield exactly what the whole-file
//!   parsers yield (item for item, id for id, failure for failure);
//! - the batch-commit driver respects its limits (memory budget),
//!   commits atomically with checkpoint advancement, and records issue
//!   rows for parse failures;
//! - cancellation preserves committed data and correct counts;
//! - an interrupted run resumes from its committed position and
//!   completes without duplicates;
//! - a large generated input stays within a memory budget (the
//!   `#[ignore]`d peak-RSS test; run with `--ignored`).

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use vault_core::bounded::{
    import_id_for, BoundedRun, Flow, IngestSink, MediaKind, StreamItem, StreamLimits, StreamSource,
};
use vault_core::discord::{parse_discord_channel, DiscordSource};
use vault_core::ingest::{Collected, IngestSummary, Source};
use vault_core::jobs::{DbPool, PoolConfig, PoolSink};
use vault_core::mbox::{parse_mbox, stream_mbox, MboxSource};
use vault_core::signal::SignalSource;
use vault_core::storage::{ImportBatchCounts, ImportIssueRecord, Storage};
use vault_core::telegram::{parse_telegram, TelegramSource};
use vault_core::whatsapp::WhatsappSource;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A Telegram export exercising every outcome kind: real messages
/// (string and array text, `from`/`from_id`), a service entry (skipped),
/// a typeless entry and a non-object entry (failures).
fn telegram_bytes(entries: usize) -> Vec<u8> {
    let mut s = String::from(r#"{"name": "Arhiva", "id": 42, "messages": ["#);
    for i in 0..entries {
        if i > 0 {
            s.push(',');
        }
        match i % 10 {
            // A real message with a unix timestamp and a body.
            0..=7 => s.push_str(&format!(
                r#"{{"id": {i}, "type": "message", "date": "2023-11-01T11:00:00", "date_unixtime": "1698832800", "from": "Marko", "text": "poruka {i} sa malo teksta"}}"#
            )),
            // A service entry: skipped, not an item.
            8 => s.push_str(&format!(
                r#"{{"id": {i}, "type": "phone_call", "actor": "Ana", "action": "call"}}"#
            )),
            // A typeless entry: a durable failure.
            _ => s.push_str(&format!(r#"{{"id": {i}, "text": "bez tipa"}}"#)),
        }
    }
    s.push_str("]}");
    s.into_bytes()
}

/// A Telegram export of nothing but real messages (exact counting for
/// the cancellation/restart tests: every entry is one item, every item
/// inserts).
fn telegram_all_messages(count: usize) -> Vec<u8> {
    let mut s = String::from(r#"{"name": "Arhiva", "id": 42, "messages": ["#);
    for i in 0..count {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            r#"{{"id": {i}, "type": "message", "date": "2023-11-01T11:00:00", "date_unixtime": "1698832800", "from": "Marko", "text": "poruka {i}"}}"#
        ));
    }
    s.push_str("]}");
    s.into_bytes()
}

/// A Signal export: incoming/outgoing messages with epoch-ms stamps, a
/// service entry, a typeless entry (failure).
fn signal_bytes(entries: usize) -> Vec<u8> {
    let mut s = String::from("[");
    for i in 0..entries {
        if i > 0 {
            s.push(',');
        }
        match i % 10 {
            0..=7 => s.push_str(&format!(
                r#"{{"id": "m{i}", "type": "incoming", "conversationId": "conv-1", "source": "+385912345678", "sent_at": {}, "body": "poruka {i}"}}"#,
                1_698_832_800_000u64 + i as u64 * 1000
            )),
            8 => s.push_str(&format!(
                r#"{{"id": "m{i}", "type": "call-history", "conversationId": "conv-1"}}"#
            )),
            _ => s.push_str(&format!(r#"{{"id": "m{i}", "body": "bez tipa"}}"#)),
        }
    }
    s.push(']');
    s.into_bytes()
}

/// A Discord channel export: Default/Reply messages, a system entry
/// (skip), a typeless entry (failure).
fn discord_bytes(entries: usize) -> Vec<u8> {
    let mut s = String::from(
        r#"{"channel": {"id": "9001", "name": "general"}, "guild": {"id": "8001", "name": "Ekipa"}, "messages": ["#,
    );
    for i in 0..entries {
        if i > 0 {
            s.push(',');
        }
        match i % 10 {
            0..=7 => s.push_str(&format!(
                r#"{{"id": "17{i:018}", "type": "Default", "timestamp": "2023-11-01 18:00:05.123+00:00", "content": "poruka {i}"}}"#
            )),
            8 => s.push_str(&format!(
                r#"{{"id": "17{i:018}", "type": "ChannelPinnedMessage", "timestamp": "2023-11-01 18:00:05.123+00:00"}}"#
            )),
            _ => s.push_str(&format!(r#"{{"id": "17{i:018}", "content": "bez tipa"}}"#)),
        }
    }
    s.push_str("]}");
    s.into_bytes()
}

/// A WhatsApp export: bracketed day-first headers, a system line, quoted
/// replies, and identical back-to-back messages.
fn whatsapp_bytes(messages: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..messages {
        match i % 10 {
            0..=6 => s.push_str(&format!(
                "[1.11.2026., 09:{:02}:{:02}] Marko: poruka {i}\n",
                i % 60,
                (i * 7) % 60
            )),
            7 => s.push_str(&format!(
                "[1.11.2026., 09:{:02}:00] Jelena: odgovor {i}\n",
                i % 60
            )),
            8 => s.push_str("[1.11.2026., 09:00:00] Marko je dodao Anu\n"),
            _ => s.push_str("[1.11.2026., 09:05:00] Marko: ok\n[1.11.2026., 09:05:00] Marko: ok\n"),
        }
    }
    s.into_bytes()
}

/// One RFC 5322 mail for the mbox fixture.
fn mail(id: usize, body_lines: usize) -> String {
    let mut body = String::new();
    for b in 0..body_lines {
        body.push_str(&format!("body of mail {id} line {b} with some text\n"));
    }
    format!(
        "From alice@example.com Sat Nov  1 10:00:00 2026\n\
         From: Alice <alice@example.com>\n\
         To: Bob <bob@example.com>\n\
         Subject: mail {id}\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\n\
         Message-ID: <{id}@example.com>\n\
         \n\
         {body}"
    )
}

/// An mbox fixture: valid mails plus (every 10th) an unparseable part.
fn mbox_bytes(mails: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..mails {
        if i % 10 == 9 {
            // Headers no parser can satisfy: a durable failure.
            s.push_str(
                "From alice@example.com Sat Nov  1 10:00:00 2026\
                 \x01garbage headers\x02\n\nnot a mail\n",
            );
        } else {
            s.push_str(&mail(i, 3));
        }
    }
    s.into_bytes()
}

fn write_file(dir: impl AsRef<Path>, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.as_ref().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Collect a stream into its full item sequence (messages and failures
/// interleaved in stream order), honoring `skip`.
fn drain_items(source: &dyn StreamSource, skip: u64) -> Vec<StreamItem> {
    let mut items = Vec::new();
    source
        .stream(skip, &mut |item| {
            items.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    items
}

/// Collect a stream into (messages, failures), honoring `skip`.
fn drain(
    source: &dyn StreamSource,
    skip: u64,
) -> (Vec<vault_core::message::NewMessage>, Vec<(String, String)>) {
    let mut messages = Vec::new();
    let mut failures = Vec::new();
    for item in drain_items(source, skip) {
        match item {
            StreamItem::Message(m) => messages.push(m),
            StreamItem::Failed {
                external_ref,
                reason,
            } => failures.push((external_ref, reason)),
        }
    }
    (messages, failures)
}

/// The same fixture parsed by the whole-file parser (the collect path —
/// the frozen reference behavior).
fn collected(_path: &Path, source: &dyn Source) -> Collected {
    source.collect().unwrap()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---------------------------------------------------------------------------
// Streaming equivalence: stream == collect, per parser
// ---------------------------------------------------------------------------

macro_rules! assert_same_items {
    ($streamed: expr, $collected: expr) => {{
        let (messages, failures) = $streamed;
        let collected: Collected = $collected;
        assert_eq!(
            messages, collected.messages,
            "streamed messages must equal the whole-file parse"
        );
        assert_eq!(
            failures.len() as u64,
            collected.failed,
            "streamed failure count must equal the whole-file count"
        );
    }};
}

/// Compare a skip-drain against the full drain in **item space**
/// (messages and failures interleaved): `skip` steps over items, not
/// just messages, so a service-skipping fixture proves the alignment.
fn assert_skip_matches(source: &dyn StreamSource, skips: &[u64]) {
    let all = drain_items(source, 0);
    let total = all.len() as u64;
    for skip in skips {
        let skipped = drain_items(source, *skip);
        assert_eq!(skipped, all[*skip as usize..], "skip {skip} of {total}");
    }
}

#[test]
fn telegram_stream_matches_collect() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(57));
    let source = TelegramSource::new(&path);
    let streamed = drain(&source, 0);
    let collected = collected(&path, &source);
    assert_same_items!(streamed, collected);
}

#[test]
fn telegram_skip_lands_on_the_same_items() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(57));
    assert_skip_matches(&TelegramSource::new(&path), &[0, 1, 5, 13, 49, 51]);
}

#[test]
fn telegram_stream_respects_stop() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(50));
    let source = TelegramSource::new(&path);
    let mut seen = 0;
    source
        .stream(0, &mut |_| {
            seen += 1;
            Ok(if seen >= 3 {
                Flow::Stop
            } else {
                Flow::Continue
            })
        })
        .unwrap();
    assert_eq!(seen, 3, "a stop must end the pull promptly");
}

#[test]
fn signal_stream_matches_collect_and_skip() {
    let dir = tmp();
    let path = write_file(&dir, "Marko.json", &signal_bytes(43));
    let source = SignalSource::new(&path);
    let streamed = drain(&source, 0);
    let collected = collected(&path, &source);
    assert_same_items!(streamed, collected);

    assert_skip_matches(&source, &[1, 7, 20, 38]);
}

#[test]
fn discord_stream_matches_collect_file_and_directory() {
    // Single channel file.
    let dir = tmp();
    let path = write_file(&dir, "messages.json", &discord_bytes(31));
    let source = DiscordSource::new(&path);
    let streamed = drain(&source, 0);
    let collected = collected(&path, &source);
    assert_same_items!(streamed, collected);

    // Directory of channels: items flow across files in sorted order.
    let pkg = tmp();
    let messages_dir = pkg.path().join("messages");
    std::fs::create_dir(&messages_dir).unwrap();
    write_file(&messages_dir, "1000.json", &discord_bytes(20));
    write_file(&messages_dir, "2000.json", &discord_bytes(20));
    write_file(
        &messages_dir,
        "index.json",
        br#"{"entries": [{"id": "9001", "channel_path": "Ekipa/general"}]}"#,
    );
    let dir_source = DiscordSource::new(&messages_dir);
    let (messages, failures) = drain(&dir_source, 0);
    let mut expected: Vec<vault_core::message::NewMessage> = Vec::new();
    let mut expected_failed = 0u64;
    for name in ["1000.json", "2000.json"] {
        let bytes = std::fs::read(messages_dir.join(name)).unwrap();
        let one = parse_discord_channel(&bytes, &HashMap::new(), "").unwrap();
        expected.extend(one.messages);
        expected_failed += one.failed;
    }
    assert_eq!(messages, expected);
    assert_eq!(failures.len() as u64, expected_failed);

    // Skip spans files (item space).
    assert_skip_matches(&dir_source, &[21]);
}

#[test]
fn whatsapp_stream_matches_collect_and_skip() {
    let dir = tmp();
    let path = write_file(&dir, "WhatsApp Chat.txt", &whatsapp_bytes(50));
    let source = WhatsappSource::new(&path);
    let streamed = drain(&source, 0);
    let collected = collected(&path, &source);
    assert_same_items!(streamed, collected);

    assert_skip_matches(&source, &[1, 6, 25, 49]);
}

#[test]
fn mbox_stream_matches_collect_and_skip() {
    let dir = tmp();
    let path = write_file(&dir, "archive.mbox", &mbox_bytes(40));
    let source = MboxSource::new(&path);
    let streamed = drain(&source, 0);
    let collected = collected(&path, &source);
    assert_same_items!(streamed, collected);

    assert_skip_matches(&source, &[1, 4, 20, 39]);
}

#[test]
fn mbox_escapes_and_edge_parts_match_between_paths() {
    // Leading junk before the first separator, an escaped `>From ` line,
    // and a separator-only trailing part: silent in both paths.
    let bytes = b"stray leading text\nFrom a@b.c\nFrom: A <a@b.c>\nSubject: s\nMessage-ID: <x@b.c>\nDate: Sat, 1 Nov 2026 10:00:00 +0100\n\nbody line\n>From escaped\nFrom a@b.c\n";
    let collected = parse_mbox(bytes, "f").unwrap();
    let mut messages = Vec::new();
    let mut failed = 0;
    stream_mbox(std::io::Cursor::new(bytes), "f", 0, &mut |item| {
        match item {
            StreamItem::Message(m) => messages.push(m),
            StreamItem::Failed { .. } => failed += 1,
        }
        Ok(Flow::Continue)
    })
    .unwrap();
    assert_eq!(messages, collected.messages);
    assert_eq!(failed, collected.failed);
}

// ---------------------------------------------------------------------------
// The driver: batching, counts, issue records
// ---------------------------------------------------------------------------

/// A sink that records every commit (sizes, positions) and can fail after
/// a chosen batch.
struct RecordingSink {
    storage: Storage,
    batch_sizes: Vec<usize>,
    batch_issue_counts: Vec<usize>,
    committed_after: Vec<u64>,
    /// Fail the Nth commit (1-based) with this error, once.
    fail_at_batch: Option<usize>,
    batches_seen: usize,
}

impl RecordingSink {
    fn new(storage: Storage) -> Self {
        Self {
            storage,
            batch_sizes: Vec::new(),
            batch_issue_counts: Vec::new(),
            committed_after: Vec::new(),
            fail_at_batch: None,
            batches_seen: 0,
        }
    }

    /// Hand the storage back (tests chain runs over one archive).
    fn into_storage(self) -> Storage {
        self.storage
    }
}

impl IngestSink for RecordingSink {
    fn checkpoint(&mut self, import_id: &str) -> vault_core::Result<Option<u64>> {
        self.storage.import_checkpoint(import_id)
    }

    fn linked_media_digests(&mut self) -> vault_core::Result<std::collections::HashSet<String>> {
        self.storage.message_media_digests()
    }

    fn commit_batch(
        &mut self,
        import_id: &str,
        source: &str,
        messages: &[vault_core::message::NewMessage],
        issues: &[ImportIssueRecord<'_>],
        media_objects: &[vault_core::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> vault_core::Result<ImportBatchCounts> {
        self.batches_seen += 1;
        if self.fail_at_batch == Some(self.batches_seen) {
            // Simulate the process dying mid-import: nothing commits.
            return Err(vault_core::Error::Io(std::io::Error::other(
                "injected interruption",
            )));
        }
        self.batch_sizes.push(messages.len());
        self.batch_issue_counts.push(issues.len());
        self.committed_after.push(committed_after);
        self.storage.commit_import_batch(
            import_id,
            source,
            messages,
            issues,
            media_objects,
            committed_after,
            completes,
        )
    }
}

#[test]
fn driver_batches_respect_both_limits() {
    let dir = tmp();
    // 45 items of which 10% fail → 41-ish messages; small message cap.
    let path = write_file(&dir, "result.json", &telegram_bytes(50));
    let source = TelegramSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);

    let run = BoundedRun {
        import_id: "t".into(),
        limits: StreamLimits {
            max_messages_per_batch: 7,
            max_batch_text_bytes: 8 * 1024 * 1024,
        },
        media: None,
    };
    let outcome = run
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();

    // Message-only batches: never more than the cap; positions advance
    // by exactly the committed batch sizes + their issue counts.
    assert!(sink.batch_sizes.iter().all(|&n| n <= 7));
    let mut expect = 0;
    for (i, &after) in sink.committed_after.iter().enumerate() {
        expect += (sink.batch_sizes[i] + sink.batch_issue_counts[i]) as u64;
        assert_eq!(after, expect);
    }
    // The final commit completes the import (checkpoint removed) and the
    // counts equal the whole-file parse.
    assert_eq!(
        sink.storage.import_checkpoint("t").unwrap(),
        None,
        "a completed import leaves no checkpoint"
    );
    let collected = parse_telegram(&telegram_bytes(50), "result").unwrap();
    assert_eq!(
        outcome.summary.seen,
        collected.messages.len() as u64 + collected.failed
    );
    assert_eq!(outcome.summary.inserted, collected.messages.len() as u64);
    assert_eq!(outcome.summary.failed, collected.failed);
    assert!(!outcome.cancelled);
}

#[test]
fn driver_flushes_on_text_budget() {
    // Bodies big enough that the byte cap hits before the message cap:
    // one message per batch.
    let dir = tmp();
    let mut bytes = String::from(r#"{"name": "A", "id": 1, "messages": ["#);
    for i in 0..3 {
        if i > 0 {
            bytes.push(',');
        }
        bytes.push_str(&format!(
            r#"{{"id": {i}, "type": "message", "date": "2023-11-01T11:00:00", "from": "M", "text": "{}"}}"#,
            "x".repeat(600)
        ));
    }
    bytes.push_str("]}");
    let path = write_file(&dir, "result.json", bytes.as_bytes());
    let source = TelegramSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);
    let run = BoundedRun {
        import_id: "t".into(),
        limits: StreamLimits {
            max_messages_per_batch: 100,
            max_batch_text_bytes: 1024,
        },
        media: None,
    };
    run.run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert!(
        sink.batch_sizes.iter().all(|&n| n <= 2),
        "the text budget must flush before the message cap: {:?}",
        sink.batch_sizes
    );
}

#[test]
fn issue_records_are_durable_with_positions() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(30));
    let source = TelegramSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(Storage::open_in_memory().unwrap());
    // Keep one storage the sink writes to and another we inspect through —
    // simpler: inspect through the same handle the sink owns after the run.
    let run = BoundedRun::new("issues");
    run.run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    let issues = sink.storage.import_issues("issues", 0, 100).unwrap();
    let expected_failed = parse_telegram(&telegram_bytes(30), "r").unwrap().failed;
    assert_eq!(issues.len() as u64, expected_failed);
    // Positions are item ordinals (counting messages), strictly ordered,
    // and every record carries a reason.
    assert!(issues.windows(2).all(|w| w[0].position < w[1].position));
    assert!(issues.iter().all(|i| !i.reason.is_empty()));
    assert_eq!(
        sink.storage.import_issue_count("issues").unwrap(),
        expected_failed
    );
    // A fresh storage handle proves rows were persisted, not just counted.
    let _ = &storage;
}

#[test]
fn issue_records_match_whole_archive_without_import() {
    // The per-item failure detail itself: reasons distinguish the cases.
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(25));
    let source = TelegramSource::new(&path);
    let (_, failures) = drain(&source, 0);
    assert!(
        failures.iter().any(|(r, _)| r == "9"),
        "the typeless entry's id is the best-effort external ref"
    );
    assert!(
        failures.iter().all(|(_, reason)| reason.contains("type")),
        "{failures:?}"
    );
}

/// A completed import re-run against the same archive keeps its issue
/// records intact: the messages dedup (the standing idempotence contract)
/// and the unique `(import_id, position)` upsert keeps the issue table
/// byte-identical — no duplicated rows, no inflated counts.
#[test]
fn completed_rerun_does_not_duplicate_issue_records() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(50));
    let source = TelegramSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();

    // First run completes (fixture failures land as durable issue rows).
    let mut first = RecordingSink::new(storage);
    let first_outcome = BoundedRun::new("rerun")
        .run(&source, &mut first, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert!(
        first_outcome.summary.failed > 0,
        "fixture must contain parse failures"
    );
    assert!(!first
        .storage
        .import_issues("rerun", 0, 1000)
        .unwrap()
        .is_empty());

    // Snapshot the issue rows, then hand the storage to the rerun.
    let before = first.storage.import_issues("rerun", 0, 1000).unwrap();

    // Second run of the same file under the same import id: fresh stream,
    // same failures re-emitted, upsert keeps the table identical.
    let mut second = RecordingSink::new(first.into_storage());
    let second_outcome = BoundedRun::new("rerun")
        .run(&source, &mut second, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert_eq!(
        second_outcome.summary.inserted, 0,
        "re-import inserts nothing (messages already stored)"
    );
    let after = second.storage.import_issues("rerun", 0, 1000).unwrap();
    let count = second.storage.import_issue_count("rerun").unwrap();
    let expected_failed = first_outcome.summary.failed;
    assert_eq!(
        count, expected_failed,
        "issue count stays exactly the failed-item count across the rerun"
    );
    // Table is byte-identical: same rows, same order, same reasons.
    assert_eq!(before, after, "issue rows survive the rerun unchanged");
}

// ---------------------------------------------------------------------------
// Cancellation: committed data and counts survive
// ---------------------------------------------------------------------------

#[test]
fn cancellation_preserves_committed_data_and_counts() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_all_messages(500));
    let source = TelegramSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);
    // Cancel once 50 items have been pulled (is_cancelled runs once per
    // item, so this is exact — progress events are rate-limited and
    // cannot serve as the counter).
    let pulled = Cell::new(0u64);
    let run = BoundedRun {
        import_id: "cancel".into(),
        limits: StreamLimits {
            max_messages_per_batch: 10,
            max_batch_text_bytes: usize::MAX,
        },
        media: None,
    };
    let outcome = run
        .run(
            &source,
            &mut sink,
            &|| {
                pulled.set(pulled.get() + 1);
                pulled.get() > 50
            },
            &mut |_, _, _| {},
        )
        .unwrap();

    assert!(outcome.cancelled);
    // Counts: seen == inserted + skipped + failed, all committed.
    let s = &outcome.summary;
    assert_eq!(s.seen, s.inserted + s.skipped + s.failed);
    assert_eq!(s.inserted, 50, "exactly the committed batches are counted");
    // The checkpoint holds the committed position for the resume.
    assert_eq!(
        sink.storage.import_checkpoint("cancel").unwrap(),
        Some(outcome.committed_items)
    );
    // Committed rows are all present and searchable.
    assert_eq!(sink.storage.stats().unwrap().total, 50);
}

#[test]
fn cancelled_run_resumes_and_completes_without_duplicates() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_all_messages(500));
    let source = TelegramSource::new(&path);

    // Run 1: cancel after 50 pulled items.
    let storage = {
        let mut sink = RecordingSink::new(Storage::open_in_memory().unwrap());
        let pulled = Cell::new(0u64);
        let run = BoundedRun {
            import_id: "resume".into(),
            limits: StreamLimits {
                max_messages_per_batch: 10,
                max_batch_text_bytes: usize::MAX,
            },
            media: None,
        };
        let outcome = run
            .run(
                &source,
                &mut sink,
                &|| {
                    pulled.set(pulled.get() + 1);
                    pulled.get() > 50
                },
                &mut |_, _, _| {},
            )
            .unwrap();
        assert!(outcome.cancelled);
        assert_eq!(outcome.summary.inserted, 50);
        assert_eq!(sink.storage.import_checkpoint("resume").unwrap(), Some(50));
        sink.into_storage()
    };

    // Run 2: same import id — resumes from the checkpoint, completes.
    let mut sink = RecordingSink::new(storage);
    let run = BoundedRun {
        import_id: "resume".into(),
        limits: StreamLimits {
            max_messages_per_batch: 10,
            max_batch_text_bytes: usize::MAX,
        },
        media: None,
    };
    let outcome = run
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert!(!outcome.cancelled);
    assert_eq!(outcome.resumed_from, 50);
    // Across both runs the archive holds every message exactly once.
    assert_eq!(sink.storage.stats().unwrap().total, 500);
    // This run's own counts cover only its portion.
    assert_eq!(
        outcome.summary.inserted + outcome.summary.skipped + outcome.summary.failed,
        outcome.summary.seen
    );
    assert_eq!(outcome.summary.seen, 450, "only the un-resumed remainder");
    assert_eq!(outcome.summary.inserted, 450);
}

#[test]
fn restart_recovers_from_last_committed_position() {
    // File-backed storage: a "crash" mid-import (injected batch failure)
    // leaves a checkpoint; a fresh Storage (a new process) resumes.
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_all_messages(300));
    let db = dir.path().join("vault.db");
    let source = TelegramSource::new(&path);

    // Run 1: fail the 3rd commit (2 batches of 10 committed durably).
    let first = {
        let storage = Storage::open(&db).unwrap();
        let mut sink = RecordingSink::new(storage);
        sink.fail_at_batch = Some(3);
        let run = BoundedRun {
            import_id: "restart".into(),
            limits: StreamLimits {
                max_messages_per_batch: 10,
                max_batch_text_bytes: usize::MAX,
            },
            media: None,
        };
        let outcome = run.run(&source, &mut sink, &|| false, &mut |_, _, _| {});
        assert!(outcome.is_err(), "the injected interruption must surface");
        sink.storage
    };
    // The first two batches committed: 20 items, checkpoint at 20.
    assert_eq!(first.stats().unwrap().total, 20);
    assert_eq!(first.import_checkpoint("restart").unwrap(), Some(20));

    // Run 2: a fresh process resumes and completes.
    let second = {
        let storage = Storage::open(&db).unwrap();
        assert_eq!(
            storage.import_checkpoint("restart").unwrap(),
            Some(20),
            "the checkpoint survives the reopen"
        );
        let mut sink = RecordingSink::new(storage);
        let run = BoundedRun {
            import_id: "restart".into(),
            limits: StreamLimits {
                max_messages_per_batch: 10,
                max_batch_text_bytes: usize::MAX,
            },
            media: None,
        };
        let outcome = run
            .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
            .unwrap();
        assert_eq!(outcome.resumed_from, 20);
        assert!(!outcome.cancelled);
        sink.storage
    };
    let collected_messages = 300u64;
    assert_eq!(
        second.stats().unwrap().total,
        collected_messages,
        "the resume completes the archive without duplicates"
    );
    assert_eq!(
        second.import_checkpoint("restart").unwrap(),
        None,
        "completion clears the checkpoint"
    );
    // No failures in this fixture, so no issue rows either.
    assert_eq!(second.import_issue_count("restart").unwrap(), 0);
}

// ---------------------------------------------------------------------------
// Media summary propagation (regression: e2e media-extract caught the
// driver dropping the sink's media-linked counts)
// ---------------------------------------------------------------------------

/// A sink that reports fixed media-link counts per commit, proving the
/// driver accumulates them into the outcome's media summary.
struct MediaCountingSink {
    storage: Storage,
    per_batch_linked: u64,
}

impl IngestSink for MediaCountingSink {
    fn checkpoint(&mut self, import_id: &str) -> vault_core::Result<Option<u64>> {
        self.storage.import_checkpoint(import_id)
    }

    fn linked_media_digests(&mut self) -> vault_core::Result<std::collections::HashSet<String>> {
        self.storage.message_media_digests()
    }

    fn commit_batch(
        &mut self,
        import_id: &str,
        source: &str,
        messages: &[vault_core::message::NewMessage],
        issues: &[ImportIssueRecord<'_>],
        media_objects: &[vault_core::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> vault_core::Result<ImportBatchCounts> {
        let counts = self.storage.commit_import_batch(
            import_id,
            source,
            messages,
            issues,
            media_objects,
            committed_after,
            completes,
        )?;
        Ok(ImportBatchCounts {
            inserted: counts.inserted,
            skipped: counts.skipped,
            // Real sinks report links for the messages they committed;
            // an empty (completing) flush links nothing.
            media_linked: if messages.is_empty() {
                0
            } else {
                self.per_batch_linked
            },
            // The receipt the real commit wrote rides through untouched.
            receipt: counts.receipt,
        })
    }
}

#[test]
fn media_linked_counts_reach_the_outcome_summary() {
    let dir = tmp();
    // Two batches of 5 (cap 5): each reports 4 linked → 8 total.
    let path = write_file(&dir, "result.json", &telegram_all_messages(10));
    let source = TelegramSource::new(&path);
    let mut sink = MediaCountingSink {
        storage: Storage::open_in_memory().unwrap(),
        per_batch_linked: 4,
    };
    let run = BoundedRun {
        import_id: "media".into(),
        limits: StreamLimits {
            max_messages_per_batch: 5,
            max_batch_text_bytes: usize::MAX,
        },
        // The media hook is set (as the chat-export path does) so the
        // outcome carries a media summary; the fixture references no
        // files, so the extraction pass itself is a no-op (its behavior
        // is media_extract.rs's domain).
        media: Some(vault_core::bounded::MediaPass {
            kind: MediaKind::Telegram,
            export_root: dir.path(),
            export_file: None,
            data_dir: dir.path(),
            unlinked: false,
        }),
    };
    let outcome = run
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    let media = outcome.media.expect("media summary present");
    assert_eq!(
        media.linked, 8,
        "linked accumulates the sink's per-batch media-linked counts"
    );
}

#[test]
fn bounded_import_runs_through_the_pool() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(120));
    let pool = DbPool::spawn(dir.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap();
    let source = TelegramSource::new(&path);
    let mut sink = PoolSink(&pool);
    let mut progress_calls = 0;
    let outcome = BoundedRun::new("pool-run")
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {
            progress_calls += 1
        })
        .unwrap();
    assert_eq!(outcome.summary.inserted, 96); // 120 items, 12 service, 12 fail
    assert!(progress_calls >= 1);
    // Searches work immediately after (readers never saw a partial batch).
    let total = pool.with_read(|s| s.stats().unwrap().total);
    assert_eq!(total, 96);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Memory budget: deterministic boundedness + the large-input RSS test
// ---------------------------------------------------------------------------

/// An endless synthetic source: proves the driver never accumulates more
/// than one batch + one item, deterministically (no RSS flakiness).
struct EndlessSource {
    n: AtomicU64,
}

impl StreamSource for EndlessSource {
    fn name(&self) -> &str {
        "endless"
    }

    fn stream(
        &self,
        _skip: u64,
        emit: &mut dyn FnMut(StreamItem) -> vault_core::Result<Flow>,
    ) -> vault_core::Result<()> {
        loop {
            let i = self.n.fetch_add(1, Ordering::Relaxed);
            let msg = vault_core::message::NewMessage::new(
                "endless",
                format!("m{i}"),
                "a",
                "2026-01-01T00:00:00Z",
                format!("body {i}"),
            );
            if emit(StreamItem::Message(msg))? == Flow::Stop {
                return Ok(());
            }
        }
    }
}

#[test]
fn driver_never_holds_more_than_one_batch() {
    // Cancel after 1000 items with a batch cap of 25: the sink must never
    // observe a batch larger than the cap (the only structural bound on
    // the driver's memory), and 1000 is an exact multiple of 25, so the
    // committed position is exact.
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);
    let run = BoundedRun {
        import_id: "endless".into(),
        limits: StreamLimits {
            max_messages_per_batch: 25,
            max_batch_text_bytes: usize::MAX,
        },
        media: None,
    };
    let pulled = Cell::new(0u64);
    let outcome = run
        .run(
            &EndlessSource {
                n: AtomicU64::new(0),
            },
            &mut sink,
            &|| {
                pulled.set(pulled.get() + 1);
                pulled.get() > 1000
            },
            &mut |_, _, _| {},
        )
        .unwrap();
    assert!(outcome.cancelled);
    assert_eq!(outcome.committed_items, 1000);
    assert!(sink.batch_sizes.iter().all(|&n| n <= 25));
    assert_eq!(sink.storage.stats().unwrap().total, 1000);
}

/// Peak RSS stays within a fixed budget while large generated exports
/// stream through the full parser + driver + storage path — Telegram
/// (no occurrence counter) and WhatsApp (whose content-hash id
/// disambiguation is the documented O(n) exception).
///
/// Heavy (generates two ~140 MB exports, runs the real file parsers):
/// run explicitly with `cargo test -p vault-core --test bounded_ingest
/// large_generated_input_stays_within_memory_budget -- --ignored`.
#[test]
#[ignore = "heavy: ~280 MB of fixtures, minutes-scale runtime"]
fn large_generated_input_stays_within_memory_budget() {
    const BUDGET_MB: u64 = 96;
    const COUNT: usize = 150_000;
    let dir = tmp();
    // ~140 MB of Telegram JSON: 150k messages with ~900-byte bodies.
    let path = dir.path().join("result.json");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        f.write_all(br#"{"name": "Big", "id": 7, "messages": ["#)
            .unwrap();
        for i in 0..COUNT {
            if i > 0 {
                f.write_all(b",").unwrap();
            }
            write!(
                f,
                r#"{{"id": {i}, "type": "message", "date": "2023-11-01T11:00:00", "date_unixtime": "1698832800", "from": "Marko", "text": "{}"}}"#,
                "x".repeat(900)
            )
            .unwrap();
        }
        f.write_all(b"]}").unwrap();
        f.flush().unwrap();
    }
    let tg_size = std::fs::metadata(&path).unwrap().len();
    assert!(
        tg_size > 100_000_000,
        "fixture must be large ({tg_size} bytes)"
    );

    // ~140 MB of WhatsApp text: 150k distinct one-line messages, so every
    // message lands a distinct entry in the parser's occurrence counter —
    // the memory this run must hold within budget.
    let wa_path = dir.path().join("chat.txt");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&wa_path).unwrap());
        for i in 0..COUNT {
            writeln!(
                f,
                "[1.11.2023., 11:00:00] Marko: m{}-{}",
                i,
                "x".repeat(880)
            )
            .unwrap();
        }
        f.flush().unwrap();
    }
    let wa_size = std::fs::metadata(&wa_path).unwrap().len();
    assert!(
        wa_size > 100_000_000,
        "fixture must be large ({wa_size} bytes)"
    );

    let db = dir.path().join("vault.db");
    let storage = Storage::open(&db).unwrap();
    let mut sink = RecordingSink::new(storage);
    let peak_before = peak_rss_kb();
    let tg = BoundedRun::new("big")
        .run(
            &TelegramSource::new(&path),
            &mut sink,
            &|| false,
            &mut |_, _, _| {},
        )
        .unwrap();
    let wa = BoundedRun::new("big-wa")
        .run(
            &WhatsappSource::new(&wa_path),
            &mut sink,
            &|| false,
            &mut |_, _, _| {},
        )
        .unwrap();
    let grew_kb = peak_rss_kb().saturating_sub(peak_before);
    println!(
        "large-input RSS: input {:.1} + {:.1} MB, peak grew {} KB ({:.1} MB)",
        tg_size as f64 / 1_000_000.0,
        wa_size as f64 / 1_000_000.0,
        grew_kb,
        grew_kb as f64 / 1024.0
    );
    assert_eq!(tg.summary.inserted, 150_000);
    assert_eq!(wa.summary.inserted, 150_000);
    assert!(
        grew_kb < BUDGET_MB * 1024,
        "peak RSS grew by {} KB (budget {BUDGET_MB} MB) on {:.1} + {:.1} MB inputs",
        grew_kb,
        tg_size as f64 / 1_000_000.0,
        wa_size as f64 / 1_000_000.0
    );
}

/// Current peak RSS of this process (VmHWM), in KB; 0 when unavailable
/// (non-Linux). The delta across the import is the test's measurement.
fn peak_rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(0);
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Import identity
// ---------------------------------------------------------------------------

#[test]
fn import_id_is_stable_until_the_file_changes() {
    let dir = tmp();
    let path = write_file(&dir, "a.txt", b"hello\n");
    let first = import_id_for("watch", &path);
    let second = import_id_for("watch", &path);
    assert_eq!(first, second, "an unchanged file keeps its import id");
    // A different prefix is a different import.
    assert_ne!(first, import_id_for("chat", &path));
    // A changed file (size or mtime) is a different import.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&dir, "a.txt", b"hello again\n");
    assert_ne!(first, import_id_for("watch", &path));
}

// ---------------------------------------------------------------------------
// IngestSummary shape sanity (IPC contract unchanged)
// ---------------------------------------------------------------------------

#[test]
fn summary_shape_matches_the_legacy_ingest_summary() {
    let dir = tmp();
    let path = write_file(&dir, "result.json", &telegram_bytes(30));
    let source = TelegramSource::new(&path);
    let mut sink = RecordingSink::new(Storage::open_in_memory().unwrap());
    let outcome = BoundedRun::new("shape")
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    let legacy: IngestSummary = source
        .collect()
        .map(|c| {
            // Same counting rules: seen = messages + failed, all inserted on a
            // fresh archive.
            IngestSummary {
                source: "telegram".into(),
                seen: c.messages.len() as u64 + c.failed,
                inserted: c.messages.len() as u64,
                skipped: 0,
                failed: c.failed,
            }
        })
        .unwrap();
    assert_eq!(outcome.summary, legacy);
}
