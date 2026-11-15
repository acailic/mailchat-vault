//! JSON Lines import integration tests: the `.jsonl` export format's
//! import counterpart. Covers row parsing (each row keeps its own
//! `source`/`external_id` identity; the exported vault `id` is ignored),
//! content-sniffed detection against the other chat formats' shapes, the
//! bounded import pipeline (batched commits through `BoundedRun`,
//! malformed lines as durable issue records with the run continuing),
//! and mixed-source files landing rows under their original namespaces.
//!
//! The export→import round-trip itself (export a seeded archive, import
//! the bytes back) lives in `tests/export.rs` with the other export
//! round-trips.

use std::path::PathBuf;

use vault_core::bounded::{BoundedRun, Flow, IngestSink, StreamItem, StreamSource};
use vault_core::chat_export::{detect_chat_format, detect_chat_format_reader, ChatFormat};
use vault_core::ingest::Source;
use vault_core::jsonl::{is_jsonl_export, JsonlSource, JSONL_SOURCE};
use vault_core::storage::Storage;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One exported row, as the JSON Lines export writes it: the full stored
/// `Message` object, autoincrement `id` included (ignored on import).
fn row(id: i64, source: &str, external_id: &str, body: &str, raw_json: &str) -> String {
    format!(
        r#"{{"id":{id},"source":"{source}","external_id":"{external_id}","channel":"chat","chat_id":"ekipa","author":"Marko","sent_at":"2026-11-01T10:00:00Z","body":"{body}","raw_json":{raw_json}}}"#
    )
}

/// A realistic mixed-source `.jsonl` export: rows from several chat
/// namespaces plus an email row, one malformed line, one blank line.
/// `raw_json` rides as the JSON *string* the export writes (the stored
/// extras serialized inside the row).
const MIXED_JSONL: &str = concat!(
    r#"{"id":11,"source":"telegram","external_id":"42/7","channel":"chat","chat_id":"Arhiva","author":"Marko","sent_at":"2026-11-01T10:00:00Z","body":"zdravo ekipa 👋","raw_json":"{\"text\":\"zdravo ekipa 👋\"}"}"#,
    "\n",
    "\n",
    r#"{"id":12,"source":"signal","external_id":"conv-1/m-2","channel":"chat","chat_id":"Marko","author":"You","sent_at":"2026-11-01T10:01:00Z","body":"jeste!","raw_json":"{\"attachments\":[]}"}"#,
    "\n",
    "{not json}\n",
    r#"{"id":13,"source":"mbox","external_id":"c@e.com","channel":"email","chat_id":"","author":"Ana <ana@e.com>","sent_at":"2026-11-01T09:00:00Z","body":"Pozdrav\n\ntekst poruke","raw_json":"{\"subject\":\"Pozdrav\"}"}"#,
    "\n",
    r#"{"id":14,"source":"telegram","external_id":"42/8","channel":"chat","chat_id":"Arhiva","author":"Jelena","sent_at":"2026-11-01T10:02:00Z","body":"vidimo se","raw_json":"{\"media\":[{\"sha256\":\"ab12\",\"path\":\"media/ab12.jpg\",\"name\":\"sl.jpg\",\"kind\":\"image\",\"size\":10}]}"}"#,
    "\n",
    r#"{"id":15,"source":"no-identity"}"#,
    "\n",
);

fn write_jsonl(name: &str, contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

// ---------------------------------------------------------------------------
// Line parsing: fidelity, ignored id, lenient decode
// ---------------------------------------------------------------------------

#[test]
fn rows_keep_their_own_identity_and_fields_verbatim() {
    let collected = vault_core::jsonl::parse_jsonl(MIXED_JSONL.as_bytes()).unwrap();
    assert_eq!(collected.messages.len(), 4);
    assert_eq!(collected.failed, 2); // "{not json}" + the identity-less row
    let first = &collected.messages[0];
    assert_eq!(first.source, "telegram");
    assert_eq!(first.external_id, "42/7");
    assert_eq!(first.channel, "chat");
    assert_eq!(first.chat_id, "Arhiva");
    assert_eq!(first.author, "Marko");
    assert_eq!(first.sent_at, "2026-11-01T10:00:00Z");
    assert_eq!(first.body, "zdravo ekipa 👋");
    assert_eq!(first.raw_json, r#"{"text":"zdravo ekipa 👋"}"#);
    // The email row: multi-line body and raw_json extras survive verbatim.
    let mail = &collected.messages[2];
    assert_eq!(mail.source, "mbox");
    assert_eq!(mail.channel, "email");
    assert_eq!(mail.body, "Pozdrav\n\ntekst poruke");
    assert_eq!(mail.raw_json, r#"{"subject":"Pozdrav"}"#);
}

#[test]
fn exported_row_id_is_ignored() {
    // The same row under two different vault autoincrement ids: import
    // yields the same identity, so a fresh vault's own ids never leak in.
    let bytes = format!(
        "{}\n{}\n",
        row(1, "telegram", "42/7", "x", r#""{}""#),
        row(9999, "telegram", "42/7", "x", r#""{}""#)
    );
    let collected = vault_core::jsonl::parse_jsonl(bytes.as_bytes()).unwrap();
    assert_eq!(collected.messages.len(), 2);
    assert_eq!(
        collected.messages[0].external_id,
        collected.messages[1].external_id
    );
}

#[test]
fn stream_matches_collect_and_skip_lands_on_items() {
    let (_dir, path) = write_jsonl("export.jsonl", MIXED_JSONL);
    let source = JsonlSource::new(&path);
    let mut streamed = Vec::new();
    source
        .stream(0, &mut |item| {
            streamed.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    let collected = source.collect().unwrap();
    assert_eq!(streamed.len(), 6); // 4 messages + 2 failures (blank lines skip)
    assert_eq!(
        streamed
            .iter()
            .filter(|i| matches!(i, StreamItem::Message(_)))
            .count(),
        collected.messages.len()
    );
    // Skip steps over items (messages and failures alike).
    let mut seen = Vec::new();
    source
        .stream(3, &mut |item| {
            seen.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(seen.len(), 3, "6 items total, skipping 3 leaves 3");
    assert!(matches!(seen[0], StreamItem::Message(_))); // item 3 is the mbox row
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[test]
fn jsonl_payload_is_detected_by_bytes_and_reader() {
    assert_eq!(
        detect_chat_format(MIXED_JSONL.as_bytes()),
        Some(ChatFormat::Jsonl)
    );
    let (_dir, path) = write_jsonl("export.jsonl", MIXED_JSONL);
    assert_eq!(
        detect_chat_format_reader(&path).unwrap(),
        Some(ChatFormat::Jsonl)
    );
    assert!(is_jsonl_export(MIXED_JSONL.as_bytes()));
}

#[test]
fn other_chat_shapes_still_win_over_jsonl() {
    // A Signal conversation export: a top-level array — not lines.
    let signal = br#"[{"id":"m1","type":"incoming","conversationId":"c","source":"+3851","sent_at":1000,"body":"x"}]"#;
    assert_eq!(detect_chat_format(signal), Some(ChatFormat::Signal));
    // A Telegram object export (has a messages array).
    let telegram = br#"{"name":"A","id":42,"messages":[{"id":1,"type":"message","date":"2026-11-01T10:00:00","from":"M","text":"hi"}]}"#;
    assert_eq!(detect_chat_format(telegram), Some(ChatFormat::Telegram));
    // Garbage and empty input: nothing.
    assert_eq!(detect_chat_format(b"just some notes\n"), None);
    assert_eq!(detect_chat_format(b""), None);
    assert!(!is_jsonl_export(b"just some notes\n"));
}

#[test]
fn resolve_stream_source_builds_the_jsonl_source() {
    let (_dir, path) = write_jsonl("subset.jsonl", MIXED_JSONL);
    let (format, file, source) =
        vault_core::chat_export::resolve_stream_source(&path, None).unwrap();
    assert_eq!(format, ChatFormat::Jsonl);
    assert_eq!(file, path);
    assert_eq!(source.name(), JSONL_SOURCE);
}

// ---------------------------------------------------------------------------
// Bounded import through BoundedRun
// ---------------------------------------------------------------------------

/// A test sink wrapping a Storage, recording every commit's sizes.
struct RecordingSink {
    storage: Storage,
    batch_sizes: Vec<usize>,
    batch_issue_counts: Vec<usize>,
}

impl RecordingSink {
    fn new(storage: Storage) -> Self {
        Self {
            storage,
            batch_sizes: Vec::new(),
            batch_issue_counts: Vec::new(),
        }
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
        issues: &[vault_core::storage::ImportIssueRecord<'_>],
        media_objects: &[vault_core::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> vault_core::Result<vault_core::storage::ImportBatchCounts> {
        self.batch_sizes.push(messages.len());
        self.batch_issue_counts.push(issues.len());
        self.storage.commit_batch(
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
fn bounded_import_stores_rows_under_their_original_namespaces() {
    let (_dir, path) = write_jsonl("subset.jsonl", MIXED_JSONL);
    let source = JsonlSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);

    let outcome = BoundedRun::new("jsonl-run")
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();

    // 4 rows stored, 2 malformed lines recorded as durable issues, the
    // run completed (nothing cancelled, no checkpoint left behind).
    assert_eq!(outcome.summary.source, JSONL_SOURCE);
    assert_eq!(outcome.summary.seen, 6);
    assert_eq!(outcome.summary.inserted, 4);
    assert_eq!(outcome.summary.failed, 2);
    assert!(!outcome.cancelled);
    assert_eq!(sink.storage.import_checkpoint("jsonl-run").unwrap(), None);
    assert_eq!(
        sink.storage
            .import_issues("jsonl-run", 0, 100)
            .unwrap()
            .len(),
        2
    );

    // Mixed-source rows land under their original namespaces — the run
    // is not re-namespaced under `jsonl`.
    let stats = sink.storage.stats().unwrap();
    assert_eq!(stats.total, 4);
    let by_source: Vec<(String, u64)> = stats
        .by_source
        .into_iter()
        .map(|s| (s.source, s.count))
        .collect();
    assert_eq!(
        by_source,
        vec![
            ("telegram".to_string(), 2),
            ("mbox".to_string(), 1),
            ("signal".to_string(), 1),
        ]
    );
}

#[test]
fn malformed_lines_are_issues_and_the_run_continues() {
    // Malformed lines sit between well-formed rows; everything after a
    // malformed line still lands, and each failure is a positioned,
    // durable issue record.
    let (_dir, path) = write_jsonl(
        "broken.jsonl",
        &format!(
            "{{not json}}\n{}\n\"a bare string\"\n{}\n",
            row(1, "telegram", "t/1", "first", r#""{}""#),
            row(2, "telegram", "t/2", "second", r#""{}""#)
        ),
    );
    let source = JsonlSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = RecordingSink::new(storage);
    let outcome = BoundedRun::new("broken")
        .run(&source, &mut sink, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert_eq!(outcome.summary.inserted, 2);
    assert_eq!(outcome.summary.failed, 2);
    let issues = sink.storage.import_issues("broken", 0, 100).unwrap();
    assert_eq!(issues.len(), 2);
    // Positions are item ordinals counting every line item.
    assert_eq!(issues[0].position, 0);
    assert_eq!(issues[1].position, 2);
    assert!(!issues.iter().any(|i| i.reason.is_empty()));
    assert_eq!(sink.storage.stats().unwrap().total, 2);
}

#[test]
fn reimport_reports_every_row_as_duplicate() {
    let (_dir, path) = write_jsonl("twice.jsonl", MIXED_JSONL);
    let source = JsonlSource::new(&path);
    let storage = Storage::open_in_memory().unwrap();
    let mut first = RecordingSink::new(storage);

    let first_outcome = BoundedRun::new("twice")
        .run(&source, &mut first, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert_eq!(first_outcome.summary.inserted, 4);

    // Same file, same import id, completed run: fresh stream, dedup on
    // each row's own (source, external_id) — everything skipped.
    let mut second = RecordingSink {
        storage: first.storage,
        batch_sizes: Vec::new(),
        batch_issue_counts: Vec::new(),
    };
    let second_outcome = BoundedRun::new("twice")
        .run(&source, &mut second, &|| false, &mut |_, _, _| {})
        .unwrap();
    assert_eq!(second_outcome.summary.inserted, 0);
    assert_eq!(second_outcome.summary.skipped, 4);
    assert_eq!(second.storage.stats().unwrap().total, 4);
}

// ---------------------------------------------------------------------------
// Preview and the shared import flow
// ---------------------------------------------------------------------------

#[test]
fn preview_reports_jsonl_kind_counts_and_media_references() {
    let (_dir, path) = write_jsonl("preview.jsonl", MIXED_JSONL);
    let preview = vault_core::preview::preview_import(&path).unwrap();
    assert_eq!(preview.kind, "jsonl");
    assert_eq!(preview.source, JSONL_SOURCE);
    assert_eq!(preview.messages_seen, 4);
    assert_eq!(preview.parse_failures, 2);
    assert!(preview.scan_complete);
    // The one row with stored `media` entries counts as referencing
    // media; absence is never claimed (media bytes are not transferred).
    assert_eq!(preview.media_referenced, 1);
    assert_eq!(preview.media_missing, 0);
    assert!(preview
        .warnings
        .iter()
        .any(|w| w.contains("could not be parsed")));
}

#[test]
fn shared_import_flow_streams_jsonl_like_any_other_chat_format() {
    // `import_bounded` is the entry the app's import job runs: a picked
    // `.jsonl` resolves, streams, and commits through the same pipeline.
    let (_dir, path) = write_jsonl(
        "flow.jsonl",
        &format!("{}\n", row(1, "telegram", "42/9", "zdravo", r#""{}""#)),
    );
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = vault_core::chat_export::import_bounded(
        &mut storage,
        &path,
        true,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1);
    assert_eq!(outcome.summary.source, JSONL_SOURCE);
    assert_eq!(storage.stats().unwrap().total, 1);
    assert_eq!(outcome.media, None, "no media pass runs for jsonl");
}
