//! Export integration tests: search results and whole chat threads to CSV,
//! JSON Lines and MBOX. Round-trips (CSV through a strict RFC 4180 reader
//! and the independent `csv` crate parser, JSON Lines through serde and a
//! fresh database, MBOX through the production `parse_mbox` importer),
//! Unicode bodies, filter fidelity, ordering, whole-thread export, and
//! large result sets that exceed every pagination bound.

use std::io::Write;

use vault_core::export::{export_chat, export_search, ExportFormat, ExportSummary};
use vault_core::mbox::parse_mbox;
use vault_core::message::{Message, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::storage::Storage;

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
        exported, paged,
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
