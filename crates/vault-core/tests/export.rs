//! Export integration tests: search results and whole chat threads to CSV,
//! JSON Lines and MBOX. Round-trips (CSV through a strict RFC 4180 reader
//! and the independent `csv` crate parser, JSON Lines through serde and a
//! fresh database, MBOX through the production `parse_mbox` importer),
//! Unicode bodies, filter fidelity, ordering, whole-thread export, and
//! large result sets that exceed every pagination bound.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use vault_core::export::{export_chat, export_search, ExportFormat, ExportSummary};
use vault_core::mbox::parse_mbox;
use vault_core::message::{Message, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::search2::SearchArchiveRequest;
use vault_core::storage::{ConnectorIdentity, NewSourceInstance, Occurrence, Storage};

/// An email-shaped row with the `raw_json` extras the mail parsers write.
/// The `Date` header is derived from `sent_at` (rendered at a fixed +01:00
/// offset) so an MBOX round-trip reproduces the same timestamps.
fn email(id: &str, subject: &str, text: &str, sent_at: &str, to: &str) -> NewMessage {
    let date_header = chrono::DateTime::parse_from_rfc3339(sent_at)
        .unwrap()
        .with_timezone(&chrono::FixedOffset::east_opt(3600).unwrap())
        .format("%a, %d %b %Y %H:%M:%S %z")
        .to_string();
    let mut m = NewMessage::new(
        "mbox",
        id,
        "Ana Sender <ana@example.com>".to_string(),
        sent_at,
        format!("{subject}\n\n{text}"),
    );
    m.chat_id = "arhiva".into();
    m.raw_json = format!(
        r#"{{"message_id":"{id}","subject":"{subject}","from":"Ana Sender <ana@example.com>","to":"{to}","date":"{date_header}"}}"#
    );
    m
}

/// Apply the writer's mboxrd escaping to a body, for comparing what a
/// re-import of an MBOX export yields: [`vault_core::mbox`] keeps
/// `>From `-escaped lines verbatim (it does not unescape), so a body line
/// starting with `From ` comes back with one leading `>`.
fn mbox_escaped(body: &str) -> String {
    body.split('\n')
        .map(|l| {
            if l.starts_with("From ") {
                format!(">{l}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A chat-shaped row (non-email channel).
fn chat(id: &str, author: &str, sent_at: &str, body: &str, chat_id: &str) -> NewMessage {
    let mut m = NewMessage::new("whatsapp", id, author, sent_at, body);
    m.channel = "chat".into();
    m.chat_id = chat_id.into();
    m
}

fn seeded(rows: Vec<NewMessage>) -> Storage {
    let s = Storage::open_in_memory().unwrap();
    for m in &rows {
        s.insert_message(m).unwrap();
    }
    s
}

fn export_to_vec(
    s: &Storage,
    query: &SearchQuery,
    format: ExportFormat,
) -> (ExportSummary, Vec<u8>) {
    let mut buf = Vec::new();
    let summary = export_search(s, query, format, &mut buf).unwrap();
    (summary, buf)
}

// ---------------------------------------------------------------------------
// Strict RFC 4180 reader (records separated by CRLF, quoted fields with
// doubled quotes). A double quote may only open a quoted field at the very
// start of a field — a quote in the middle of an unquoted field is
// malformed and rejected, so the old prefixed-cell malformation (the
// mitigation apostrophe written outside the quotes) cannot parse.
// ---------------------------------------------------------------------------

fn parse_csv(bytes: &[u8]) -> Vec<Vec<String>> {
    let text = String::from_utf8(bytes.to_vec()).expect("csv must be utf-8");
    assert!(
        text.ends_with("\r\n"),
        "csv file must end with a CRLF record terminator"
    );
    let mut records = Vec::new();
    let mut field = String::new();
    let mut record: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => {
                    assert!(
                        field.is_empty(),
                        "double quote inside an unquoted field: not RFC 4180 (a quote may only open a quoted field at the start of a field)"
                    );
                    in_quotes = true;
                }
                ',' => {
                    record.push(std::mem::take(&mut field));
                }
                '\r' => {
                    assert_eq!(chars.next(), Some('\n'), "CR must be followed by LF");
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                other => field.push(other),
            }
        }
    }
    assert!(!in_quotes, "unterminated quoted field");
    assert!(record.is_empty(), "trailing partial record");
    records
}

/// Compare two message lists ignoring row ids (re-import assigns new ones).
fn assert_same_ignoring_ids(a: &[Message], b: &[Message]) {
    let strip = |m: &Message| {
        let mut m = m.clone();
        m.id = 0;
        m
    };
    let sa: Vec<_> = a.iter().map(strip).collect();
    let sb: Vec<_> = b.iter().map(strip).collect();
    assert_eq!(sa, sb);
}

/// Clear `source_key` before a comparison: the portable source identity
/// is export-only metadata (populated by the JSON export streams,
/// empty on rows built by every other query), so it is deliberately
/// absent from the paginated search API's rows.
fn strip_export_only_source_key(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|mut m| {
            m.source_key = String::new();
            m
        })
        .collect()
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

#[test]
fn csv_neutralizes_formula_prefix_cells() {
    // A message body crafted as a spreadsheet formula exports inert:
    // write_csv_row prefixes cells beginning with =, +, - or @ with an
    // apostrophe, so opening the CSV in Excel/LibreOffice shows text
    // instead of executing the cell.
    let s = seeded(vec![NewMessage::new(
        "mbox",
        "formula@example.com",
        "Ana Sender <ana@example.com>",
        "2026-11-03T09:00:00Z",
        "=cmd|' /C calc'!A0 formula injection probe",
    )]);
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Csv);
    let records = parse_csv(&bytes);
    assert_eq!(records.len(), 2, "header + the one message");
    let body_cell = &records[1][6];
    assert!(
        body_cell.starts_with('\''),
        "formula body must be apostrophe-prefixed, got {body_cell:?}"
    );
    assert!(
        body_cell.contains("=cmd|' /C calc'!A0"),
        "body content preserved after the mitigation prefix"
    );
}

#[test]
fn csv_round_trip_preserves_every_field() {
    let rows = vec![
        email(
            "plain@example.com",
            "Kickoff",
            "the vault kickoff is confirmed",
            "2026-11-01T09:00:00Z",
            "Bob <bob@example.com>",
        ),
        NewMessage::new(
            "mbox",
            "quoted, \"fancy\"",
            "Smith, Ana <ana@example.com>",
            "2026-11-02T09:00:00Z",
            "commas, \"quotes\"\nand newlines\r\nend",
        ),
        NewMessage::new("telegram", "empty-fields", "", "", ""),
    ];
    let s = seeded(rows.clone());

    let (summary, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Csv);
    assert_eq!(
        summary,
        ExportSummary {
            format: ExportFormat::Csv,
            written: 3,
            skipped: 0,
        }
    );

    let records = parse_csv(&bytes);
    assert_eq!(records[0], vault_core::export::CSV_COLUMNS.to_vec());
    assert_eq!(records.len(), 4);

    // Field-by-field identity with the stored rows (browse order is
    // newest-first by sent_at, then insertion).
    assert_eq!(records[1][0], "mbox");
    assert_eq!(records[1][1], "quoted, \"fancy\"");
    assert_eq!(records[1][5], "2026-11-02T09:00:00Z");
    assert_eq!(records[1][6], "commas, \"quotes\"\nand newlines\r\nend");
    assert_eq!(records[3][0], "telegram");
    assert_eq!(records[3][6], "");
    assert_eq!(records[3][7], "{}");
}

/// Regression (see the CSV export malformation): a cell that gets the
/// formula-mitigation apostrophe *and* needs RFC 4180 quoting must carry
/// the apostrophe inside the quotes. The exported bytes are parsed by the
/// independent `csv` crate in its default non-flexible mode — any record
/// whose column count drifts from the 8-column header is a hard error —
/// so a strict third-party reader, not one duplicating the writer's
/// assumptions, is the judge.
#[test]
fn csv_prefixed_cells_that_need_quoting_are_valid_rfc4180() {
    let bodies = [
        "- hello, world", // the reported reproduction
        "= comma, after the prefix",
        "+ \"quote\" after the prefix",
        "- newline\nafter the prefix",
        "@ CRLF\r\nafter the prefix",
        "= unicode čćžšđ 👋 中文, prefix",
        "+ comma, \"quote\"\nand newline after the prefix",
        "@ lone @ prefix, no quotes", // prefixed but unquotable
        "plain unprefixed, control",
        "plain \"quoted\" unprefixed control",
        "plain\nunprefixed newline control",
    ];
    let s = seeded(
        bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                NewMessage::new(
                    "mbox",
                    format!("prefix-{i}@example.com"),
                    "Ana Sender <ana@example.com>",
                    "2026-11-03T09:00:00Z",
                    (*body).to_string(),
                )
            })
            .collect(),
    );
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Csv);

    // The apostrophe sits inside the quotes in the raw bytes for the
    // reproduction case (the old writer glued it outside, before the `"`).
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("\"'- hello, world\""),
        "prefixed cell must quote the apostrophe into the field, got: {text}"
    );

    let mut reader = csv::ReaderBuilder::new().from_reader(bytes.as_slice());
    let headers = reader.headers().expect("header record parses").clone();
    assert_eq!(headers, vault_core::export::CSV_COLUMNS.to_vec());

    let mut parsed = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record.unwrap_or_else(|e| panic!("record {i} is not valid RFC 4180: {e}"));
        assert_eq!(record.len(), 8, "record {i} must keep exactly 8 columns");
        parsed.push(record);
    }
    assert_eq!(parsed.len(), bodies.len(), "one record per message");

    // Every body cell carries the mitigation apostrophe and the original
    // text, matched by the row's external_id (order-independent).
    for (i, body) in bodies.iter().enumerate() {
        let id = format!("prefix-{i}@example.com");
        let record = parsed
            .iter()
            .find(|r| r[1] == id)
            .unwrap_or_else(|| panic!("record for external_id {id} missing"));
        let expected = if matches!(body.as_bytes().first(), Some(b'=' | b'+' | b'-' | b'@')) {
            format!("'{body}")
        } else {
            (*body).to_string()
        };
        assert_eq!(record[6], expected, "body cell of {id}");
    }
}

// ---------------------------------------------------------------------------
// JSON Lines
// ---------------------------------------------------------------------------

#[test]
fn json_lines_round_trip_is_bit_identical_into_a_fresh_archive() {
    let rows = vec![
        email(
            "u@example.com",
            "Pozdrav čćžšđ",
            "unicode body: hvala 👋 中文 mixed with ASCII",
            "2026-11-01T09:00:00Z",
            "Bob <bob@example.com>",
        ),
        chat(
            "w-1",
            "marko",
            "2026-11-01T10:00:00Z",
            "zdravo ekipa 🎉",
            "arhiva",
        ),
    ];
    let s = seeded(rows);
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Json);

    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);

    // Re-import into a fresh archive through the public insert path and
    // compare row-for-row, exactly like the integrity round-trip suite.
    let target = Storage::open_in_memory().unwrap();
    for line in &lines {
        let m: Message = serde_json::from_str(line).unwrap();
        let new = vault_core::message::NewMessage {
            source: m.source.clone(),
            external_id: m.external_id.clone(),
            channel: m.channel.clone(),
            chat_id: m.chat_id.clone(),
            author: m.author.clone(),
            sent_at: m.sent_at.clone(),
            body: m.body.clone(),
            raw_json: m.raw_json.clone(),
            // `Message` exposes only the M1.1 columns; the M1.3 provenance
            // fields stay empty so the re-import applies the same legacy
            // grouping as the original rows.
            conversation_key: String::new(),
            attachments: Vec::new(),
            reply_refs: Vec::new(),
            original_timestamp: String::new(),
            timezone: String::new(),
            time_confidence: String::new(),
            source_label: String::new(),
        };
        target.insert_message(&new).unwrap();
    }
    let mut before: Vec<Message> = Vec::new();
    s.search_export_stream(&SearchQuery::default(), &mut |m| {
        before.push(m.clone());
        Ok(())
    })
    .unwrap();
    let mut after: Vec<Message> = Vec::new();
    target
        .search_export_stream(&SearchQuery::default(), &mut |m| {
            after.push(m.clone());
            Ok(())
        })
        .unwrap();
    assert_same_ignoring_ids(&before, &after);
}

/// The full round-trip through the **importer**: export a seeded archive
/// as JSON Lines, feed the bytes back through the app's own bounded
/// import (`import_bounded`, the path the chat-export job runs), and
/// demand row-for-row identity — then the idempotence re-imports the
/// dedup contract promises.
#[test]
fn json_lines_import_round_trip_through_the_importer() {
    let rows = vec![
        email(
            "rt@example.com",
            "Pozdrav čćžšđ",
            "unicode body: hvala 👋 中文",
            "2026-11-01T09:00:00Z",
            "Bob <bob@example.com>",
        ),
        chat(
            "w-1",
            "marko",
            "2026-11-01T10:00:00Z",
            "zdravo ekipa 🎉",
            "arhiva",
        ),
        chat("t-9", "ana", "2026-11-01T11:00:00Z", "vidimo se", "arhiva"),
    ];
    let mut s = seeded(rows);
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Json);

    // The exported bytes as a picked file: one well-formed row per line.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subset.jsonl");
    std::fs::write(&path, &bytes).unwrap();

    // Import into a fresh archive through the bounded pipeline.
    let mut target = Storage::open_in_memory().unwrap();
    let outcome = vault_core::chat_export::import_bounded(
        &mut target,
        &path,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 3);
    assert_eq!(outcome.summary.failed, 0);

    let mut before: Vec<Message> = Vec::new();
    s.search_export_stream(&SearchQuery::default(), &mut |m| {
        before.push(m.clone());
        Ok(())
    })
    .unwrap();
    let mut after: Vec<Message> = Vec::new();
    target
        .search_export_stream(&SearchQuery::default(), &mut |m| {
            after.push(m.clone());
            Ok(())
        })
        .unwrap();
    assert_same_ignoring_ids(&before, &after);

    // Re-import the same file: every row reports duplicate, nothing new.
    let again = vault_core::chat_export::import_bounded(
        &mut target,
        &path,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(again.summary.inserted, 0);
    assert_eq!(again.summary.skipped, 3);
    assert_eq!(target.stats().unwrap().total, 3);

    // And into the original archive: nothing duplicates there either —
    // rows keep their own (source, external_id) identity.
    let original = vault_core::chat_export::import_bounded(
        &mut s,
        &path,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(original.summary.inserted, 0);
    assert_eq!(original.summary.skipped, 3);
    assert_eq!(s.stats().unwrap().total, 3);
}

/// A2b: a completed bounded import carries the receipt its completing
/// commit wrote — run-total new counts, the failing record, observed
/// bounds — on the outcome (the shape the job runner serializes
/// verbatim), while a cancelled run claims no receipt, and job results
/// written before the field existed still deserialize.
#[test]
fn import_outcome_carries_the_completion_receipt() {
    let rows = vec![
        chat("r-1", "marko", "2026-11-01T10:00:00Z", "prva", "arhiva"),
        chat("r-2", "ana", "2026-11-02T11:00:00Z", "druga", "arhiva"),
    ];
    let s = seeded(rows);
    let (_, mut bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Json);
    // One line that cannot be a row (no source/external_id identity):
    // a durable failure record, not a run abort.
    bytes.extend_from_slice(b"{\"id\": \"orphan\"}\n");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("receipt.jsonl");
    std::fs::write(&path, &bytes).unwrap();

    let mut target = Storage::open_in_memory().unwrap();
    let outcome = vault_core::chat_export::import_bounded(
        &mut target,
        &path,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 2);
    assert_eq!(outcome.summary.failed, 1);
    let receipt = outcome
        .receipt
        .as_ref()
        .expect("a completed run carries its receipt");
    assert_eq!(receipt.new_records, 2);
    assert!(receipt.failed_records > 0, "the failing record is counted");
    assert_eq!(receipt.failed_records, 1);
    assert_eq!(
        receipt.observed_min.as_deref(),
        Some("2026-11-01T10:00:00Z")
    );
    assert_eq!(
        receipt.observed_max.as_deref(),
        Some("2026-11-02T11:00:00Z")
    );

    // The receipt rides the serialized outcome (the job runner and the
    // direct commands serialize this shape verbatim), and a job result
    // written before the field existed still deserializes — an absent
    // receipt means None, never an error.
    let value = serde_json::to_value(vault_core::chat_export::ChatImportOutcome::from(
        outcome.clone(),
    ))
    .unwrap();
    assert_eq!(
        value
            .pointer("/receipt/failed_records")
            .and_then(|v| v.as_u64()),
        Some(1),
        "the serialized outcome carries the receipt: {value}"
    );
    let mut legacy = value;
    legacy.as_object_mut().unwrap().remove("receipt");
    let parsed: vault_core::chat_export::ChatImportOutcome =
        serde_json::from_value(legacy).unwrap();
    assert!(parsed.receipt.is_none());

    // A cancelled run claims no receipt: committed work stays exactly as
    // the checkpoint left it, but no completion is reported that did
    // not happen.
    let mut other = Storage::open_in_memory().unwrap();
    let cancelled = vault_core::chat_export::import_bounded(
        &mut other,
        &path,
        false,
        None,
        &|| true,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert!(cancelled.cancelled);
    assert!(cancelled.receipt.is_none());
}

// ---------------------------------------------------------------------------
// MBOX
// ---------------------------------------------------------------------------

#[test]
fn mbox_round_trip_through_the_production_parser() {
    let rows = vec![
        email(
            "one@example.com",
            "Quarterly report",
            "The vault migration plan was approved.",
            "2026-11-01T09:00:00Z",
            "Bob <bob@example.com>",
        ),
        email(
            "two@example.com",
            "Pozdrav čćžšđ",
            "unicode body: hvala 👋 中文\nFrom the second line\nbye",
            "2026-11-02T09:00:00Z",
            "Bob <bob@example.com>",
        ),
    ];
    let s = seeded(rows.clone());
    let (summary, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Mbox);
    assert_eq!(summary.written, 2);
    assert_eq!(summary.skipped, 0);

    let collected = parse_mbox(&bytes, "arhiva").unwrap();
    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 2);

    // Same rows, same order as written (browse order was newest-first, so
    // the re-imported list is reversed relative to `rows`).
    let parsed: Vec<Message> = collected
        .messages
        .iter()
        .map(|n| {
            let mut m = Message {
                id: 0,
                source: n.source.clone(),
                source_key: String::new(),
                external_id: n.external_id.clone(),
                channel: n.channel.clone(),
                chat_id: n.chat_id.clone(),
                author: n.author.clone(),
                sent_at: n.sent_at.clone(),
                body: n.body.clone(),
                raw_json: n.raw_json.clone(),
            };
            m.raw_json = "{}".into(); // extras are rebuilt, not copied
            m
        })
        .collect();
    let expected: Vec<Message> = rows
        .iter()
        .rev()
        .map(|n| Message {
            id: 0,
            source: n.source.clone(),
            source_key: String::new(),
            external_id: n.external_id.clone(),
            channel: n.channel.clone(),
            chat_id: n.chat_id.clone(),
            author: n.author.clone(),
            sent_at: n.sent_at.clone(),
            // The re-imported body is the mboxrd-escaped form of the
            // original (see `mbox_escaped`): content intact, embedded
            // `From ` lines carry their escape marker.
            body: mbox_escaped(&n.body),
            raw_json: "{}".into(),
        })
        .collect();
    assert_same_ignoring_ids(&parsed, &expected);

    // The generated Message-IDs (dedup keys) survive, and the mboxrd escape
    // of the embedded "From " line keeps the message from splitting.
    assert_eq!(collected.messages[0].external_id, "two@example.com");
    assert!(collected.messages[0].body.contains(">From the second line"));
    assert_eq!(collected.messages[1].external_id, "one@example.com");
}

#[test]
fn mbox_skips_non_email_rows_and_counts_them() {
    let rows = vec![
        chat(
            "w-1",
            "marko",
            "2026-11-01T10:00:00Z",
            "chat row one",
            "arhiva",
        ),
        email(
            "m@example.com",
            "Mail",
            "email row",
            "2026-11-01T09:00:00Z",
            "Bob <bob@example.com>",
        ),
        chat(
            "w-2",
            "jelena",
            "2026-11-01T11:00:00Z",
            "chat row two",
            "arhiva",
        ),
    ];
    let s = seeded(rows);
    let (summary, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Mbox);
    assert_eq!(summary.written, 1);
    assert_eq!(summary.skipped, 2);
    let collected = parse_mbox(&bytes, "x").unwrap();
    assert_eq!(collected.messages.len(), 1);
    assert_eq!(collected.messages[0].external_id, "m@example.com");
}

#[test]
fn mbox_reimports_into_a_working_archive() {
    let rows = vec![email(
        "r@example.com",
        "Subject here",
        "body text with unicode 👋",
        "2026-11-05T12:34:56Z",
        "To Someone <to@example.com>",
    )];
    let s = seeded(rows);
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Mbox);

    let fresh = Storage::open_in_memory().unwrap();
    let collected = parse_mbox(&bytes, "arhiva").unwrap();
    for m in &collected.messages {
        fresh.insert_message(m).unwrap();
    }
    let hits = fresh
        .search_filtered("unicode", None, None, None, false, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].body, "Subject here\n\nbody text with unicode 👋");
}

// ---------------------------------------------------------------------------
// Filters and ordering
// ---------------------------------------------------------------------------

#[test]
fn export_respects_filters_and_text_query() {
    let rows = vec![
        email(
            "a@e.com",
            "A",
            "alpha beta",
            "2026-11-01T09:00:00Z",
            "t@e.com",
        ),
        email(
            "b@e.com",
            "B",
            "alpha only",
            "2026-11-02T09:00:00Z",
            "t@e.com",
        ),
        chat(
            "w-1",
            "marko",
            "2026-11-03T09:00:00Z",
            "alpha from marko",
            "c1",
        ),
        chat("w-2", "ana", "2026-11-04T09:00:00Z", "alpha from ana", "c1"),
    ];
    let s = seeded(rows);

    // Text query: only matching rows, in relevance order (not tested for
    // exact rank order here — just membership and count).
    let (summary, _) = export_to_vec(
        &s,
        &SearchQuery {
            text: "beta".into(),
            ..SearchQuery::default()
        },
        ExportFormat::Csv,
    );
    assert_eq!(summary.written, 1);

    // Source filter.
    let (summary, _) = export_to_vec(
        &s,
        &SearchQuery {
            sources: vec!["whatsapp".into()],
            ..SearchQuery::default()
        },
        ExportFormat::Csv,
    );
    assert_eq!(summary.written, 2);

    // Author filter (case-insensitive substring) + date bound together.
    let (summary, _) = export_to_vec(
        &s,
        &SearchQuery {
            author: "MARKO".into(),
            from_date: "2026-11-01".into(),
            to_date: "2026-11-03".into(),
            ..SearchQuery::default()
        },
        ExportFormat::Csv,
    );
    assert_eq!(summary.written, 1);

    // A text query combined with a source filter narrows further.
    let (summary, _) = export_to_vec(
        &s,
        &SearchQuery {
            text: "alpha".into(),
            sources: vec!["mbox".into()],
            ..SearchQuery::default()
        },
        ExportFormat::Csv,
    );
    assert_eq!(summary.written, 2);
}

#[test]
fn browse_export_is_newest_first_and_matches_pagination_order() {
    let rows: Vec<NewMessage> = (0..12)
        .map(|i| {
            email(
                &format!("m{i}@e.com"),
                "S",
                &format!("body number {i}"),
                &format!("2026-11-{:02}T09:00:00Z", i + 1),
                "t@e.com",
            )
        })
        .collect();
    let s = seeded(rows);

    // Streamed export order must equal the concatenation of search pages.
    let query = SearchQuery {
        text: "body".into(),
        limit: 5,
        ..SearchQuery::default()
    };
    let mut exported: Vec<Message> = Vec::new();
    s.search_export_stream(&query, &mut |m| {
        exported.push(m.clone());
        Ok(())
    })
    .unwrap();
    let mut paged: Vec<Message> = Vec::new();
    let mut offset = 0;
    loop {
        let page = s
            .search_page(&SearchQuery {
                offset,
                ..query.clone()
            })
            .unwrap();
        if page.hits.is_empty() {
            break;
        }
        paged.extend(page.hits.into_iter().map(|h| h.message));
        offset += 5;
    }
    assert_eq!(exported.len(), 12);
    assert_eq!(paged.len(), 12);
    assert_eq!(
        strip_export_only_source_key(exported),
        paged,
        "export order must match UI pagination order"
    );

    // Browse mode (empty text) is newest-first.
    let mut exported = Vec::new();
    s.search_export_stream(&SearchQuery::default(), &mut |m| {
        exported.push(m.clone());
        Ok(())
    })
    .unwrap();
    let ids: Vec<&str> = exported.iter().map(|m| m.external_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "m11@e.com",
            "m10@e.com",
            "m9@e.com",
            "m8@e.com",
            "m7@e.com",
            "m6@e.com",
            "m5@e.com",
            "m4@e.com",
            "m3@e.com",
            "m2@e.com",
            "m1@e.com",
            "m0@e.com"
        ]
    );
}

// ---------------------------------------------------------------------------
// Large sets (beyond every pagination bound)
// ---------------------------------------------------------------------------

#[test]
fn large_result_set_exports_every_row() {
    // 6_000 rows, half matching a distinctive term — far beyond the 200-row
    // MAX_LIMIT page bound; export must still cover the whole set, quickly.
    let s = Storage::open_in_memory().unwrap();
    for i in 0..6_000u32 {
        let body = if i % 2 == 0 {
            format!("zebra body {i} with some padding text to look real")
        } else {
            format!("other body {i} entirely different words")
        };
        let mut m = NewMessage::new(
            "gen",
            format!("gen-{i}"),
            format!("author-{i}"),
            format!("2026-{:02}-{:02}T09:00:00Z", i % 12 + 1, i % 28 + 1),
            body,
        );
        m.channel = if i % 3 == 0 {
            "email".into()
        } else {
            "chat".into()
        };
        s.insert_message(&m).unwrap();
    }

    let query = SearchQuery {
        text: "zebra".into(),
        ..SearchQuery::default()
    };
    let expected_total = s.search_page(&query).unwrap().total;
    assert_eq!(expected_total, 3_000);

    // Counting sink: no file needed, just prove the stream covers all rows.
    let mut seen = 0;
    let streamed = s
        .search_export_stream(&query, &mut |_| {
            seen += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(streamed, 3_000);
    assert_eq!(seen, 3_000);

    // Full CSV export: header + 3_000 rows.
    let mut buf = Vec::new();
    let summary = export_search(&s, &query, ExportFormat::Csv, &mut buf).unwrap();
    assert_eq!(summary.written, 3_000);
    assert_eq!(parse_csv(&buf).len(), 3_001);

    // Full JSON export: 3_000 lines, and every line parses as a Message.
    let mut buf = Vec::new();
    let summary = export_search(&s, &query, ExportFormat::Json, &mut buf).unwrap();
    assert_eq!(summary.written, 3_000);
    let json = String::from_utf8(buf).unwrap();
    let lines: Vec<&str> = json.lines().collect();
    assert_eq!(lines.len(), 3_000);
    for line in lines {
        serde_json::from_str::<Message>(line).expect("every jsonl line is a Message");
    }

    // MBOX of the full archive (no filters): email rows only, chats skipped.
    let mut buf = Vec::new();
    let summary = export_search(&s, &SearchQuery::default(), ExportFormat::Mbox, &mut buf).unwrap();
    assert_eq!(summary.written, 2_000);
    assert_eq!(summary.skipped, 4_000);
    assert_eq!(parse_mbox(&buf, "x").unwrap().messages.len(), 2_000);
}

// ---------------------------------------------------------------------------
// Whole-chat export
// ---------------------------------------------------------------------------

#[test]
fn chat_export_writes_the_whole_thread_oldest_first() {
    let mut rows = Vec::new();
    for i in 0..30 {
        rows.push(chat(
            &format!("w-{i}"),
            &format!("member-{i}"),
            &format!("2026-11-01T{:02}:00:00Z", i),
            &format!("message {i}"),
            "ekipa",
        ));
    }
    // A different thread that must not leak into the export.
    rows.push(chat(
        "x-1",
        "outsider",
        "2026-11-01T15:00:00Z",
        "other thread",
        "drugi",
    ));
    let s = seeded(rows);

    let middle = {
        let all = s
            .search_export_stream(&SearchQuery::default(), &mut |_| Ok(()))
            .unwrap();
        assert_eq!(all, 31);
        let page = s
            .search_page(&SearchQuery {
                author: "member-15".into(),
                ..SearchQuery::default()
            })
            .unwrap();
        page.hits[0].message.id
    };

    let mut buf = Vec::new();
    let summary = export_chat(&s, middle, ExportFormat::Csv, &mut buf)
        .unwrap()
        .expect("message exists");
    assert_eq!(summary.written, 30);

    let records = parse_csv(&buf);
    assert_eq!(records.len(), 31); // header + 30
    assert_eq!(records[1][4], "member-0"); // oldest first
    assert_eq!(records[30][4], "member-29");
    assert!(records[1..].iter().all(|r| r[3] == "ekipa"));

    // The thread lookup is by (source, chat_id): rows of other threads,
    // even in the same source, are excluded.
    assert!(!records.iter().any(|r| r[1] == "x-1"));
}

#[test]
fn chat_export_of_a_threadless_message_is_single_row() {
    let mut m = NewMessage::new(
        "mbox",
        "lone@e.com",
        "a",
        "2026-11-01T09:00:00Z",
        "standalone",
    );
    m.raw_json = r#"{"subject":"S"}"#.into();
    let s = seeded(vec![m]);
    let id = s.search_page(&SearchQuery::default()).unwrap().hits[0]
        .message
        .id;

    let mut buf = Vec::new();
    let summary = export_chat(&s, id, ExportFormat::Json, &mut buf)
        .unwrap()
        .expect("message exists");
    assert_eq!(summary.written, 1);
    assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 1);
}

#[test]
fn chat_export_of_unknown_id_is_none() {
    let s = Storage::open_in_memory().unwrap();
    let mut buf = Vec::new();
    assert!(export_chat(&s, 999, ExportFormat::Csv, &mut buf)
        .unwrap()
        .is_none());
    assert!(buf.is_empty());
}

#[test]
fn chat_export_mbox_skips_non_email_thread_rows() {
    let mut email_row = email(
        "e-1@e.com",
        "S",
        "email msg",
        "2026-11-01T10:00:00Z",
        "t@e.com",
    );
    email_row.chat_id = "mixed".into();
    // Threads are per (source, chat_id): join the chat rows' source.
    email_row.source = "whatsapp".into();
    let rows = vec![
        chat("w-0", "marko", "2026-11-01T09:00:00Z", "chat msg", "mixed"),
        email_row,
        chat(
            "w-1",
            "jelena",
            "2026-11-01T11:00:00Z",
            "chat msg 2",
            "mixed",
        ),
    ];
    let s = seeded(rows);
    let id = s
        .search_page(&SearchQuery {
            author: "ana".into(),
            ..SearchQuery::default()
        })
        .unwrap()
        .hits[0]
        .message
        .id;

    let mut buf = Vec::new();
    let summary = export_chat(&s, id, ExportFormat::Mbox, &mut buf)
        .unwrap()
        .expect("message exists");
    assert_eq!(summary.written, 1);
    assert_eq!(summary.skipped, 2);
    let collected = parse_mbox(&buf, "mixed").unwrap();
    assert_eq!(collected.messages.len(), 1);
    assert_eq!(collected.messages[0].external_id, "e-1@e.com");
    assert_eq!(collected.messages[0].chat_id, "mixed");
}

// ---------------------------------------------------------------------------
// Conversation-identity whole-chat export (provider keys, renames)
// ---------------------------------------------------------------------------

/// One Telegram export chat keyed by its provider id, the fixture shape
/// the conversation suite uses (crates/vault-core/tests/conversations.rs).
const TG_EXPORT: &str = r#"{"name": "Staro ime", "id": 42, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Ana", "text": "poruka jedan"},
    {"id": 2, "type": "message", "date_unixtime": "1730432500", "from": "Ana", "text": "poruka dva"},
    {"id": 3, "type": "message", "date_unixtime": "1730432600", "from": "Ana", "text": "poruka tri"}]}"#;
// The same chat (provider id 42) re-exported under a new display name.
const TG_EXPORT_RENAMED: &str = r#"{"name": "Novo ime", "id": 42, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Ana", "text": "poruka jedan"},
    {"id": 2, "type": "message", "date_unixtime": "1730432500", "from": "Ana", "text": "poruka dva"},
    {"id": 3, "type": "message", "date_unixtime": "1730432600", "from": "Ana", "text": "poruka tri"}]}"#;
// A different chat sharing the first export's display name.
const TG_EXPORT_SAME_LABEL: &str = r#"{"name": "Staro ime", "id": 77, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Bane", "text": "drugi chat"}]}"#;

/// Write one Telegram export file and return its path (the tempdir is
/// forgotten, keeping the file alive for the storage's lifetime).
fn tg_export(file: &str, json: &str) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file);
    std::fs::write(&path, json.as_bytes()).unwrap();
    std::mem::forget(dir);
    path
}

/// Ingest `json` as a Telegram export into `storage`.
fn tg_ingest(storage: &Storage, file: &str, json: &str) {
    vault_core::ingest::ingest(
        storage,
        &vault_core::telegram::TelegramSource::new(tg_export(file, json)),
    )
    .unwrap();
}

/// The row id of the one message whose body contains `needle`.
fn id_by_body(storage: &Storage, needle: &str) -> i64 {
    storage
        .search_filtered(&format!("\"{needle}\""), None, None, None, false, 10)
        .unwrap()
        .into_iter()
        .find(|m| m.body.contains(needle))
        .unwrap_or_else(|| panic!("no message with {needle}"))
        .id
}

/// Export one anchor's whole chat as JSON Lines and return the summary
/// plus the parsed rows.
fn chat_rows(storage: &Storage, anchor: i64) -> (ExportSummary, Vec<Message>) {
    let mut buf = Vec::new();
    let summary = export_chat(storage, anchor, ExportFormat::Json, &mut buf)
        .unwrap()
        .expect("message exists");
    let rows: Vec<Message> = String::from_utf8(buf)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    (summary, rows)
}

#[test]
fn chat_export_of_a_provider_key_conversation_writes_every_message() {
    // A Telegram chat keyed by its provider id carries a conversation
    // with no legacy chat key: the legacy display-name thread lookup
    // found nothing for these rows and the export came out empty.
    // Conversation identity must export the whole chat, oldest first.
    let storage = Storage::open_in_memory().unwrap();
    tg_ingest(&storage, "result.json", TG_EXPORT);
    assert_eq!(storage.stats().unwrap().total, 3);

    // Anchor on a mid-thread message.
    let (summary, rows) = chat_rows(&storage, id_by_body(&storage, "poruka dva"));
    assert_eq!(summary.written, 3, "the whole conversation exports");
    let bodies: Vec<&str> = rows.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["poruka jedan", "poruka dva", "poruka tri"]);
}

#[test]
fn chat_export_after_a_rename_stays_whole() {
    // Two exports of the same chat (provider id 42) under two display
    // names: one conversation, three rows after dedup — and an export
    // anchored on a row stored under the old name still covers all of
    // it, because identity, not the label, selects the thread.
    let storage = Storage::open_in_memory().unwrap();
    tg_ingest(&storage, "a.json", TG_EXPORT);
    tg_ingest(&storage, "b.json", TG_EXPORT_RENAMED);
    assert_eq!(storage.stats().unwrap().total, 3);

    let (summary, rows) = chat_rows(&storage, id_by_body(&storage, "poruka jedan"));
    assert_eq!(summary.written, 3);
    let bodies: Vec<&str> = rows.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["poruka jedan", "poruka dva", "poruka tri"]);
}

#[test]
fn chat_export_of_same_label_conversations_stays_distinct() {
    // Chats 42 and 77 share the display name "Staro ime": grouping by
    // display name (the legacy key) would mix them. Conversation
    // identity must keep both exports to their own messages.
    let storage = Storage::open_in_memory().unwrap();
    tg_ingest(&storage, "a.json", TG_EXPORT);
    tg_ingest(&storage, "b.json", TG_EXPORT_SAME_LABEL);
    assert_eq!(storage.stats().unwrap().total, 4);

    let (summary, rows) = chat_rows(&storage, id_by_body(&storage, "drugi chat"));
    assert_eq!(summary.written, 1);
    let bodies: Vec<&str> = rows.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, vec!["drugi chat"]);

    let (summary, rows) = chat_rows(&storage, id_by_body(&storage, "poruka jedan"));
    assert_eq!(summary.written, 3);
    assert!(!rows.iter().any(|m| m.body.contains("drugi chat")));
}

// ---------------------------------------------------------------------------
// Portable source identity in the whole-chat JSON Lines export
// ---------------------------------------------------------------------------

/// One connector-owned (IMAP) source instance — the only production
/// write path that stores rows under a connector-owned instance — with
/// one mail row sharing `external_id` across calls, the same-label
/// collision shape of tests/export_v2.rs. Returns the instance id.
fn imap_mail(storage: &Storage, connector_key: &str, external_id: &str, body: &str) -> String {
    let instance = storage
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Shared mailbox",
            connector_key,
            config_json: "{}",
        })
        .unwrap();
    let m = NewMessage::new("imap", external_id, "Ana", "2026-01-01T12:00:00Z", body);
    storage
        .insert_mail_message(
            &instance,
            &m,
            "Subject",
            &ConnectorIdentity {
                namespace: "imap",
                external_key: external_id,
                revision_hash: &format!("revision-{connector_key}"),
            },
            &Occurrence {
                scope: "INBOX",
                provider_item_id: "1",
                uidvalidity: Some(1),
            },
        )
        .unwrap();
    instance
}

/// A file-backed storage plus a second raw connection on the same
/// database (WAL allows the concurrent writer), the canonical-table
/// fixture pattern of tests/conversations.rs.
fn scratch_storage(name: &str) -> (Storage, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(format!("{name}.db"));
    let storage = Storage::open(&db).unwrap();
    std::mem::forget(dir);
    let raw = Connection::open(&db).unwrap();
    (storage, raw)
}

/// Give `instance`'s messages their own conversation — the canonical
/// shape `resolve_email_thread` writes. Connector mail is stored
/// conversationless today, so the whole-thread export fixture for
/// connector-owned instances needs the row attached here.
fn attach_conversation(raw: &Connection, instance: &str, message_ids: &[i64]) {
    let conversation = format!("conv-{instance}");
    raw.execute(
        "INSERT INTO conversations
             (id, source_instance_id, provider_key, label, kind, confidence)
         VALUES (?1, ?2, ?3, 'Shared mailbox', 'email', 'verified')",
        params![&conversation, instance, format!("email:thread:{instance}")],
    )
    .unwrap();
    for id in message_ids {
        raw.execute(
            "UPDATE archive_messages SET conversation_id = ?1 WHERE id = ?2",
            params![&conversation, id],
        )
        .unwrap();
    }
}

/// The P1 regression on the whole-conversation entry point: two
/// connector-owned instances share a display label and a mail
/// `Message-ID`. Before the portable source identity reached this
/// stream, both exported rows carried only `source: "Shared mailbox"`
/// and the JSON Lines importer deduped the second conversation onto the
/// first — a successful transfer that silently dropped one instance's
/// message. With `source_key` both conversation exports round-trip
/// through the production bounded importer with both bodies retained
/// and stay idempotent.
#[test]
fn chat_jsonl_export_keeps_same_label_instances_distinct_through_the_bounded_importer() {
    let (storage, raw) = scratch_storage("chat-jsonl-source-key");
    let mut anchors = Vec::new();
    for key in ["account-a", "account-b"] {
        let instance = imap_mail(
            &storage,
            key,
            "same-message-id",
            &format!("body from {key}"),
        );
        let id = id_by_body(&storage, &format!("body from {key}"));
        attach_conversation(&raw, &instance, &[id]);
        anchors.push(id);
    }
    assert_eq!(storage.stats().unwrap().total, 2);

    // Export each conversation (whole-thread branch) as JSON Lines.
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    let mut exported_keys = Vec::new();
    for (index, anchor) in anchors.iter().enumerate() {
        let mut buf = Vec::new();
        let summary = export_chat(&storage, *anchor, ExportFormat::Json, &mut buf)
            .unwrap()
            .expect("message exists");
        assert_eq!(summary.written, 1, "one row per conversation");
        let rows: Vec<Message> = String::from_utf8(buf.clone())
            .unwrap()
            .lines()
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(rows[0].source, "Shared mailbox");
        exported_keys.push(rows[0].source_key.clone());
        let path = dir.path().join(format!("conversation-{index}.jsonl"));
        std::fs::write(&path, buf).unwrap();
        files.push(path);
    }
    assert_eq!(
        exported_keys,
        vec!["instance:imap:account-a", "instance:imap:account-b"],
        "each conversation exports under its own machine key"
    );

    // Import both files, one after the other, into an empty vault
    // through the production bounded path.
    let mut target = Storage::open_in_memory().unwrap();
    for path in &files {
        let outcome = vault_core::chat_export::import_bounded(
            &mut target,
            path,
            false,
            None,
            &|| false,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            (
                outcome.summary.inserted,
                outcome.summary.skipped,
                outcome.summary.failed
            ),
            (1, 0, 0),
            "{} inserts; none collapses onto the other",
            path.display()
        );
    }
    assert_eq!(target.stats().unwrap().total, 2);

    // Both bodies survive, each under its own destination instance,
    // and the instances carry the exporting vault's display label.
    let instances = target.list_source_instances().unwrap();
    assert_eq!(instances.len(), 2, "one destination instance per key");
    assert!(instances
        .iter()
        .all(|i| i.kind == "legacy" && i.label == "Shared mailbox"));
    let mut bodies = Vec::new();
    for instance in &instances {
        let page = target
            .search_archive(&SearchArchiveRequest {
                cursor: None,
                source_ids: vec![instance.id.clone()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total, 1);
        let body = target
            .message_detail(page.hits[0].id)
            .unwrap()
            .map(|detail| detail.body)
            .unwrap_or_default();
        bodies.push(body);
    }
    bodies.sort();
    assert_eq!(
        bodies,
        vec!["body from account-a", "body from account-b"],
        "both bodies retained under distinct sources"
    );

    // Re-importing each file inserts nothing and skips its row.
    for path in &files {
        let again = vault_core::chat_export::import_bounded(
            &mut target,
            path,
            false,
            None,
            &|| false,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            (
                again.summary.inserted,
                again.summary.skipped,
                again.summary.failed
            ),
            (0, 1, 0)
        );
    }
    assert_eq!(target.stats().unwrap().total, 2);

    // The cycle is stable: re-exporting each message from the
    // destination (its rows are standalone, so this walks the export's
    // single-row branch) emits the same portable keys again.
    let mut rekeys = Vec::new();
    for body in ["body from account-a", "body from account-b"] {
        let (_, rows) = chat_rows(&target, id_by_body(&target, body));
        rekeys.push(rows[0].source_key.clone());
    }
    rekeys.sort();
    assert_eq!(
        rekeys, exported_keys,
        "destination instances export under the same machine keys"
    );
}

/// The same connector-owned message exported through a search stream
/// and through the chat export yields the same nonempty portable source
/// identity — the standalone (conversationless) row included, since
/// connector mail has no conversation yet.
#[test]
fn chat_export_rows_carry_the_same_source_key_as_the_search_export() {
    let storage = Storage::open_in_memory().unwrap();
    imap_mail(&storage, "account-a", "parity-1@example.com", "parity body");

    let (_, bytes) = export_to_vec(&storage, &SearchQuery::default(), ExportFormat::Json);
    let search_rows: Vec<Message> = String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(search_rows.len(), 1);
    assert_eq!(search_rows[0].source_key, "instance:imap:account-a");

    let (summary, exported) = chat_rows(&storage, id_by_body(&storage, "parity body"));
    assert_eq!(summary.written, 1, "standalone rows export as one row");
    assert!(!exported[0].source_key.is_empty());
    assert_eq!(
        exported[0].source_key, search_rows[0].source_key,
        "search and chat exports agree on the portable identity"
    );
    assert_eq!(exported[0].source, "Shared mailbox");
}

/// Renaming an instance's display label never changes its exported
/// identity: re-registering the connector with a new label and
/// re-exporting yields the same key, only the label moves.
#[test]
fn renaming_a_source_label_leaves_the_chat_export_identity_unchanged() {
    let storage = Storage::open_in_memory().unwrap();
    imap_mail(&storage, "account-a", "rename-1@example.com", "rename body");
    let id = id_by_body(&storage, "rename body");

    let (_, before) = chat_rows(&storage, id);
    assert_eq!(before[0].source, "Shared mailbox");

    storage
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Renamed mailbox",
            connector_key: "account-a",
            config_json: "{}",
        })
        .unwrap();
    let (_, after) = chat_rows(&storage, id);
    assert_eq!(after[0].source, "Renamed mailbox");
    assert_eq!(
        after[0].source_key, before[0].source_key,
        "the machine key does not move with the label"
    );
    assert_eq!(after[0].source_key, "instance:imap:account-a");
}

// ---------------------------------------------------------------------------
// Empty results and format parsing
// ---------------------------------------------------------------------------

#[test]
fn empty_result_set_writes_only_the_header_or_nothing() {
    let s = Storage::open_in_memory().unwrap();
    let query = SearchQuery {
        text: "nothing".into(),
        ..SearchQuery::default()
    };
    let (summary, bytes) = export_to_vec(&s, &query, ExportFormat::Csv);
    assert_eq!(summary.written, 0);
    assert_eq!(parse_csv(&bytes).len(), 1);

    let (_, bytes) = export_to_vec(&s, &query, ExportFormat::Json);
    assert!(bytes.is_empty());

    let (_, bytes) = export_to_vec(&s, &query, ExportFormat::Mbox);
    assert!(bytes.is_empty());
}

#[test]
fn export_writes_what_it_reads_via_write() {
    // The generic `Write` bound: exporting through a cursor and through a
    // file produce identical bytes.
    let rows = vec![email(
        "f@e.com",
        "S",
        "b",
        "2026-11-01T09:00:00Z",
        "t@e.com",
    )];
    let s = seeded(rows);
    let mut a = Vec::new();
    export_search(&s, &SearchQuery::default(), ExportFormat::Csv, &mut a).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    {
        let mut file = std::fs::File::create(&path).unwrap();
        export_search(&s, &SearchQuery::default(), ExportFormat::Csv, &mut file).unwrap();
        file.flush().unwrap();
    }
    let b = std::fs::read(&path).unwrap();
    assert_eq!(a, b);
}

#[test]
fn sink_error_propagates_out_of_the_stream() {
    let rows = vec![email(
        "p@e.com",
        "S",
        "b",
        "2026-11-01T09:00:00Z",
        "t@e.com",
    )];
    let s = seeded(rows);
    let err = s
        .search_export_stream(&SearchQuery::default(), &mut |_| {
            Err(vault_core::Error::InvalidExport("stop".into()))
        })
        .unwrap_err();
    assert!(err.to_string().contains("stop"));
}

// ---------------------------------------------------------------------------
// Signal media rows ride every export format like email attachments
// (the media entries the storage half links into `raw_json` must travel
// the ordinary email path verbatim: same eight CSV columns, same JSON
// Lines row, same MBOX skip — no Signal-specific format anywhere)
// ---------------------------------------------------------------------------

/// Real small files committed with the repo (the same fixtures
/// tests/media_extract.rs drives its Signal harness with).
const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// A Signal conversation export (`sigtop export-messages -f json` shape:
/// raw message objects in a top-level array): a text message, a stored
/// photo, a stored document plus a name-less voice-note reference, and a
/// referenced-but-absent photo whose quote nests another attachment.
const SIGNAL_MEDIA_JSON: &str = r#"[
  { "id": "m1", "type": "incoming", "conversationId": "conv-1",
    "source": "+385912345678.2", "sent_at": 1698832800000, "body": "zdravo" },
  { "id": "m2", "type": "incoming", "conversationId": "conv-1",
    "source": "+385912345678.2", "sent_at": 1698832900000, "body": "",
    "attachments": [
      { "fileName": "signal-2023-11-01-110050.jpg", "contentType": "image/jpeg", "size": 284119 }
    ] },
  { "id": "m3", "type": "outgoing", "conversationId": "conv-1",
    "sent_at": 1698833000000, "body": "",
    "attachments": [
      { "fileName": "report-2023.pdf", "contentType": "application/pdf" },
      { "contentType": "audio/ogg" }
    ] },
  { "id": "m4", "type": "incoming", "conversationId": "conv-1",
    "source": "+385912345678.2", "sent_at": 1698833100000, "body": "vidi sliku",
    "attachments": [ { "fileName": "gone.jpg", "contentType": "image/jpeg" } ],
    "quote": { "text": "zdravo", "attachments": [ { "fileName": "quoted.jpg" } ] } }
]"#;

/// One temp workspace per test: a Signal `export/` tree and a `data/`
/// app-data dir the media pass stores into (the harness pattern of
/// tests/media_extract.rs — nothing touches the repo).
struct SignalSandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl SignalSandbox {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        std::fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn export_dir(&self) -> PathBuf {
        let dir = self.root.join("export");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    /// Copy a fixture into the export tree at `rel`.
    fn put(&self, rel: &str, fixture_name: &str) {
        let target = self.export_dir().join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(MEDIA_FIXTURES).join(fixture_name), &target).unwrap();
    }

    /// Import the Signal export with the media pass into `storage`, like
    /// the chat-export job does for a picked export folder.
    fn import(&self, storage: &mut Storage) -> vault_core::chat_export::ChatImportOutcome {
        vault_core::chat_export::import_bounded(
            storage,
            &self.export_dir(),
            true,
            Some(&self.data_dir()),
            &|| false,
            &mut |_, _, _| {},
        )
        .map(vault_core::chat_export::ChatImportOutcome::from)
        .unwrap()
    }
}

/// The Signal media fixture imported into in-memory storage: four rows,
/// two stored media entries (photo, document), one honest missing entry.
fn signal_media_storage(tag: &str) -> (SignalSandbox, Storage) {
    let sb = SignalSandbox::new(tag);
    sb.put("attachments/signal-2023-11-01-110050.jpg", "photo.jpg");
    sb.put("attachments/report-2023.pdf", "plan.pdf");
    std::fs::write(sb.export_dir().join("Marko.json"), SIGNAL_MEDIA_JSON).unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = sb.import(&mut storage);
    assert_eq!(outcome.summary.inserted, 4);
    assert_eq!(outcome.summary.source, "signal");
    let media = outcome.media.expect("media pass ran");
    // m2 (photo) + m3 (document) stored; m4's gone.jpg honestly missing;
    // m3's name-less voice note is not a resolvable reference.
    assert_eq!(media.referenced, 3);
    assert_eq!(media.stored, 2);
    assert_eq!(media.missing, 1);
    assert_eq!(media.linked, 3);
    (sb, storage)
}

/// Every stored row's `raw_json` by `external_id`, exactly as the export
/// streams read it.
fn stored_raw_json(storage: &Storage) -> HashMap<String, String> {
    let mut map = HashMap::new();
    storage
        .search_export_stream(&SearchQuery::default(), &mut |m| {
            map.insert(m.external_id.clone(), m.raw_json.clone());
            Ok(())
        })
        .unwrap();
    map
}

/// The row id of the message with this `external_id`.
fn row_id_of(storage: &Storage, external_id: &str) -> i64 {
    storage
        .search_page(&SearchQuery {
            limit: 100,
            ..SearchQuery::default()
        })
        .unwrap()
        .hits
        .into_iter()
        .find(|h| h.message.external_id == external_id)
        .unwrap_or_else(|| panic!("message {external_id} not stored"))
        .message
        .id
}

#[test]
fn csv_export_carries_signal_raw_json_byte_identical() {
    let (_sb, s) = signal_media_storage("signal-csv");
    let stored = stored_raw_json(&s);

    let (summary, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Csv);
    assert_eq!(
        summary,
        ExportSummary {
            format: ExportFormat::Csv,
            written: 4,
            skipped: 0,
        }
    );

    // The strict RFC 4180 reader parses every Signal row into the stable
    // eight columns — no Signal-specific columns, no drift.
    let records = parse_csv(&bytes);
    assert_eq!(records[0], vault_core::export::CSV_COLUMNS.to_vec());
    assert_eq!(records.len(), 5, "header + four messages");
    assert!(
        records.iter().all(|r| r.len() == 8),
        "every record keeps exactly 8 columns"
    );

    // The raw_json cell is byte-identical to what the archive stores,
    // for every row — media entries included.
    for (id, raw) in &stored {
        let record = records
            .iter()
            .find(|r| &r[1] == id)
            .unwrap_or_else(|| panic!("record for {id} missing"));
        assert_eq!(
            &record[7], raw,
            "raw_json cell of {id} must be byte-identical"
        );
        assert_eq!(&record[0], "signal", "source column of {id}");
        assert_eq!(&record[2], "chat", "channel column of {id}");
    }

    // The stored photo entry rode the cell: digest, store path and the
    // verbatim attachment fields next to it.
    let m2: serde_json::Value = serde_json::from_str(&stored["conv-1/m2"]).unwrap();
    let entry = &m2["media"][0];
    assert_eq!(entry["name"], "signal-2023-11-01-110050.jpg");
    assert_eq!(entry["kind"], "image");
    let sha = entry["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64, "sha256 hex digest");
    assert_eq!(entry["path"], format!("media/{sha}.jpg"));
    assert_eq!(
        m2["attachments"][0]["fileName"],
        "signal-2023-11-01-110050.jpg"
    );

    // The referenced-but-absent file is the honest missing entry — it
    // rides the cell like a stored one, never dropped.
    let m4: serde_json::Value = serde_json::from_str(&stored["conv-1/m4"]).unwrap();
    assert_eq!(m4["media"][0]["name"], "gone.jpg");
    assert_eq!(m4["media"][0]["missing"], true);
    assert_eq!(m4["media"][0]["reason"], "not-in-export");

    // The name-less voice-note reference stays in the verbatim
    // attachments array; it is not a resolvable reference, so no media
    // entry exists for it — one media entry for the named document.
    let m3: serde_json::Value = serde_json::from_str(&stored["conv-1/m3"]).unwrap();
    assert_eq!(m3["attachments"].as_array().unwrap().len(), 2);
    assert_eq!(m3["media"].as_array().unwrap().len(), 1);
    assert_eq!(m3["media"][0]["name"], "report-2023.pdf");
    assert_eq!(m3["media"][0]["kind"], "document");
}

#[test]
fn whole_chat_csv_export_carries_signal_media_verbatim() {
    // The whole-thread export seam (conversation identity) must carry the
    // same rows the search export does — media entries and all.
    let (_sb, s) = signal_media_storage("signal-chat-csv");
    let stored = stored_raw_json(&s);

    let anchor = row_id_of(&s, "conv-1/m2");
    let mut buf = Vec::new();
    let summary = export_chat(&s, anchor, ExportFormat::Csv, &mut buf)
        .unwrap()
        .expect("message exists");
    assert_eq!(summary.written, 4);
    assert_eq!(summary.skipped, 0);

    let records = parse_csv(&buf);
    assert_eq!(records.len(), 5, "header + the whole conversation");
    // Oldest → newest, the thread order.
    let ids: Vec<&str> = records[1..].iter().map(|r| r[1].as_str()).collect();
    assert_eq!(
        ids,
        vec!["conv-1/m1", "conv-1/m2", "conv-1/m3", "conv-1/m4"]
    );
    for record in &records[1..] {
        assert_eq!(
            &record[7],
            stored.get(record[1].as_str()).unwrap(),
            "raw_json cell of {} must be byte-identical",
            record[1]
        );
    }
}

#[test]
fn json_lines_signal_rows_reimport_row_for_row() {
    let (_sb, s) = signal_media_storage("signal-jsonl");
    let (_, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Json);

    let text = String::from_utf8(bytes.clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4);

    // Re-import into a fresh archive through the public insert path,
    // exactly like the email rows' round-trip.
    let target = Storage::open_in_memory().unwrap();
    for line in &lines {
        let m: Message = serde_json::from_str(line).unwrap();
        assert_eq!(m.source, "signal");
        target
            .insert_message(&NewMessage {
                source: m.source.clone(),
                external_id: m.external_id.clone(),
                channel: m.channel.clone(),
                chat_id: m.chat_id.clone(),
                author: m.author.clone(),
                sent_at: m.sent_at.clone(),
                body: m.body.clone(),
                raw_json: m.raw_json.clone(),
                conversation_key: String::new(),
                attachments: Vec::new(),
                reply_refs: Vec::new(),
                original_timestamp: String::new(),
                timezone: String::new(),
                time_confidence: String::new(),
                source_label: String::new(),
            })
            .unwrap();
    }
    let mut before: Vec<Message> = Vec::new();
    s.search_export_stream(&SearchQuery::default(), &mut |m| {
        before.push(m.clone());
        Ok(())
    })
    .unwrap();
    let mut after: Vec<Message> = Vec::new();
    target
        .search_export_stream(&SearchQuery::default(), &mut |m| {
            after.push(m.clone());
            Ok(())
        })
        .unwrap();
    assert_same_ignoring_ids(&before, &after);

    // Media references stay references: the stored entries keep their
    // digest and store path, the missing entry stays missing — nothing
    // duplicates and no bytes move (the row is the reference).
    let raw_by_id = |rows: &[Message]| -> HashMap<String, String> {
        rows.iter()
            .map(|m| (m.external_id.clone(), m.raw_json.clone()))
            .collect()
    };
    let raw_before = raw_by_id(&before);
    let raw_after = raw_by_id(&after);
    for id in ["conv-1/m2", "conv-1/m3", "conv-1/m4"] {
        assert_eq!(
            raw_before.get(id),
            raw_after.get(id),
            "media entries of {id} must survive the round-trip verbatim"
        );
    }

    // And the exported lines import back into the source archive through
    // the bounded importer without duplicating a single Signal row.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signal.jsonl");
    std::fs::write(&path, bytes).unwrap();
    let mut s = s;
    let again = vault_core::chat_export::import_bounded(
        &mut s,
        &path,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(again.summary.inserted, 0);
    assert_eq!(again.summary.skipped, 4);
    assert_eq!(s.stats().unwrap().total, 4);
}

#[test]
fn mbox_export_skips_signal_rows_and_counts_them() {
    // MBOX carries email-channel rows only: Signal rows are skipped and
    // counted in `ExportSummary::skipped`, identically to other chat
    // rows — never reformatted, never silently dropped.
    let (_sb, s) = signal_media_storage("signal-mbox");
    s.insert_message(&email(
        "mixed@e.com",
        "Mejl",
        "email row in the mixed archive",
        "2026-11-01T09:00:00Z",
        "t@e.com",
    ))
    .unwrap();

    let (summary, bytes) = export_to_vec(&s, &SearchQuery::default(), ExportFormat::Mbox);
    assert_eq!(summary.written, 1);
    assert_eq!(summary.skipped, 4);

    let collected = parse_mbox(&bytes, "x").unwrap();
    assert_eq!(collected.messages.len(), 1);
    assert_eq!(collected.messages[0].external_id, "mixed@e.com");

    // No Signal content leaks into the MBOX bytes: neither bodies nor
    // media references — the rows are skipped, not translated.
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("vidi sliku"));
    assert!(!text.contains("signal-2023-11-01-110050.jpg"));
    assert!(!text.contains("gone.jpg"));
}
