//! Search/export parity through the public v2 API, including source ids
//! whose display labels collide and filters absent from the legacy API.

use vault_core::export::{export_search, export_search_v2, ExportFormat, ExportSummary};
use vault_core::message::{Message, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::search2::{SearchArchiveRequest, SearchSort};
use vault_core::storage::{ConnectorIdentity, NewSourceInstance, Occurrence, Storage};

fn message(source: &str, id: &str, date: &str, body: &str) -> NewMessage {
    let mut m = NewMessage::new(source, id, "Ana", date, body);
    m.time_confidence = if date.is_empty() {
        "missing"
    } else {
        "explicit_offset"
    }
    .into();
    m.timezone = "UTC".into();
    m
}

fn export_json(storage: &Storage, request: &SearchArchiveRequest) -> (ExportSummary, Vec<Message>) {
    let mut out = Vec::new();
    let summary = export_search_v2(storage, request, ExportFormat::Json, &mut out).unwrap();
    let rows = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (summary, rows)
}

#[test]
fn instance_ids_keep_same_named_sources_independent_and_legacy_export_compatible() {
    let storage = Storage::open_in_memory().unwrap();
    let mut sources = Vec::new();
    for key in ["account-a", "account-b"] {
        let source = storage
            .ensure_source_instance(&NewSourceInstance {
                kind: "imap",
                label: "Shared mailbox",
                connector_key: key,
                config_json: "{}",
            })
            .unwrap();
        let m = message("imap", "same-message-id", "2026-01-01T12:00:00Z", key);
        storage
            .insert_mail_message(
                &source,
                &m,
                "Subject",
                &ConnectorIdentity {
                    namespace: "imap",
                    external_key: "same-message-id",
                    revision_hash: "revision",
                },
                &Occurrence {
                    scope: "INBOX",
                    provider_item_id: "1",
                    uidvalidity: Some(1),
                },
            )
            .unwrap();
        sources.push(source);
    }
    for (index, source) in sources.iter().enumerate() {
        let request = SearchArchiveRequest {
            source_ids: vec![source.clone()],
            ..Default::default()
        };
        let page = storage.search_archive(&request).unwrap();
        assert_eq!(page.total, 1);
        let (summary, exported) = export_json(&storage, &request);
        assert_eq!(summary.written, 1);
        assert_eq!(exported[0].id, page.hits[0].id);
        assert_eq!(exported[0].body, ["account-a", "account-b"][index]);
    }
    assert_eq!(
        export_json(
            &storage,
            &SearchArchiveRequest {
                source_ids: sources,
                ..Default::default()
            }
        )
        .0
        .written,
        2,
    );
    assert_eq!(
        export_json(
            &storage,
            &SearchArchiveRequest {
                source_ids: vec!["unknown-instance".into()],
                ..Default::default()
            }
        )
        .0
        .written,
        0,
        "unknown instance ids never broaden the export",
    );
    let mut legacy = Vec::new();
    let summary = export_search(
        &storage,
        &SearchQuery {
            sources: vec!["Shared mailbox".into()],
            ..Default::default()
        },
        ExportFormat::Json,
        &mut legacy,
    )
    .unwrap();
    assert_eq!(
        summary.written, 2,
        "legacy callers keep label union semantics"
    );
}

#[test]
fn all_v2_filters_and_timezone_bounds_survive_export_while_pagination_is_ignored() {
    let storage = Storage::open_in_memory().unwrap();
    let mut rows = Vec::new();
    for (id, date, author, attached, conversation) in [
        ("first", "2026-07-14T22:15:00Z", "Ana", true, "contract"),
        ("second", "2026-07-15T21:59:00Z", "ANA", true, "contract"),
        ("too-early", "2026-07-14T21:59:00Z", "Ana", true, "contract"),
        ("too-late", "2026-07-15T22:00:00Z", "Ana", true, "contract"),
        ("no-file", "2026-07-15T12:00:00Z", "Ana", false, "contract"),
        (
            "wrong-author",
            "2026-07-15T12:00:00Z",
            "Bojan",
            true,
            "contract",
        ),
        (
            "wrong-conversation",
            "2026-07-15T12:00:00Z",
            "Ana",
            true,
            "another",
        ),
    ] {
        let mut m = message("mbox", id, date, "contract accepted");
        m.author = author.into();
        m.conversation_key = format!("email:thread:{conversation}");
        if attached {
            m.raw_json =
                r#"{"media":[{"name":"contract.pdf","kind":"document","missing":true}]}"#.into();
        }
        rows.push(m);
    }
    storage
        .commit_import_batch("selected-import", "mbox", &rows, &[], &[], 0, true)
        .unwrap();
    let mut other_import = rows[0].clone();
    other_import.external_id = "other-import".into();
    storage
        .commit_import_batch("other-import", "mbox", &[other_import], &[], &[], 0, true)
        .unwrap();
    let mut wrong_source = rows[0].clone();
    wrong_source.source = "demo".into();
    storage.insert_message(&wrong_source).unwrap();
    let source = storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|s| s.label == "mbox")
        .unwrap();
    let conversation = storage
        .search_archive(&SearchArchiveRequest {
            text: "contract".into(),
            source_ids: vec![source.id.clone()],
            ..Default::default()
        })
        .unwrap()
        .hits
        .into_iter()
        .find(|m| m.author == "ANA")
        .unwrap()
        .conversation_id;
    let request = SearchArchiveRequest {
        text: "contract".into(),
        source_ids: vec![source.id],
        conversation_id: conversation,
        import_job_id: Some("selected-import".into()),
        author: "ana".into(),
        from_date: Some("2026-07-15".into()),
        to_date: Some("2026-07-15".into()),
        timezone: "Europe/Belgrade".into(),
        has_attachment: Some(true),
        time_assumed: None,
        grouping_unverified: None,
        files_missing: None,
        sort: SearchSort::Newest,
        limit: Some(1),
        offset: Some(1),
    };
    let page = storage.search_archive(&request).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.hits.len(), 1);
    let (summary, exported) = export_json(&storage, &request);
    assert_eq!(summary.written, 2);
    assert_eq!(
        exported
            .iter()
            .map(|m| m.external_id.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
    assert_eq!(exported[1].id, page.hits[0].id);
    let without_files = SearchArchiveRequest {
        has_attachment: Some(false),
        ..request
    };
    assert_eq!(
        export_json(&storage, &without_files)
            .1
            .iter()
            .map(|m| m.external_id.as_str())
            .collect::<Vec<_>>(),
        ["no-file"]
    );
}

#[test]
fn exports_match_relevance_and_newest_order_including_undated_messages() {
    let storage = Storage::open_in_memory().unwrap();
    for (id, date, body) in [
        ("old", "2026-01-01T12:00:00Z", "needle needle needle"),
        (
            "new",
            "2026-02-01T12:00:00Z",
            "needle lots of surrounding text",
        ),
        ("undated", "", "needle"),
        ("same-date", "2026-01-01T12:00:00Z", "needle needle"),
    ] {
        storage
            .insert_message(&message("mbox", id, date, body))
            .unwrap();
    }
    for text in ["", "needle"] {
        for sort in [SearchSort::Relevance, SearchSort::Newest] {
            let request = SearchArchiveRequest {
                text: text.into(),
                sort,
                ..Default::default()
            };
            let page = storage.search_archive(&request).unwrap();
            let (_, exported) = export_json(&storage, &request);
            assert_eq!(
                exported.iter().map(|m| m.id).collect::<Vec<_>>(),
                page.hits.iter().map(|m| m.id).collect::<Vec<_>>()
            );
            if text.is_empty() || sort == SearchSort::Newest {
                assert_eq!(exported.last().unwrap().external_id, "undated");
            }
        }
    }
}

#[test]
fn all_formats_preserve_the_filtered_set_and_mbox_reports_chat_skips() {
    let storage = Storage::open_in_memory().unwrap();
    let mail = message("mbox", "mail", "2026-01-01T12:00:00Z", "mail body");
    storage.insert_message(&mail).unwrap();
    let mut chat = message("whatsapp", "chat", "2026-01-01T13:00:00Z", "chat body");
    chat.channel = "chat".into();
    storage.insert_message(&chat).unwrap();
    for (format, written, skipped) in [
        (ExportFormat::Csv, 2, 0),
        (ExportFormat::Json, 2, 0),
        (ExportFormat::Mbox, 1, 1),
    ] {
        let mut out = Vec::new();
        let summary =
            export_search_v2(&storage, &SearchArchiveRequest::default(), format, &mut out).unwrap();
        assert_eq!((summary.written, summary.skipped), (written, skipped));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("mail body"));
        assert_eq!(text.contains("chat body"), format != ExportFormat::Mbox);
    }
}

#[test]
fn export_is_not_capped_at_a_search_page_and_propagates_sink_failures() {
    let storage = Storage::open_in_memory().unwrap();
    let rows: Vec<_> = (0..251)
        .map(|i| {
            message(
                "mbox",
                &format!("m{i}"),
                "2026-01-01T12:00:00Z",
                "all pages",
            )
        })
        .collect();
    storage
        .commit_import_batch("large", "mbox", &rows, &[], &[], 0, true)
        .unwrap();
    let request = SearchArchiveRequest {
        limit: Some(1),
        offset: Some(250),
        ..Default::default()
    };
    assert_eq!(storage.search_archive(&request).unwrap().hits.len(), 1);
    assert_eq!(export_json(&storage, &request).0.written, 251);
    let mut seen = 0;
    let error = storage
        .search_archive_export_stream(&request, &mut |_| {
            seen += 1;
            Err(vault_core::Error::InvalidExport("sink stopped".into()))
        })
        .unwrap_err();
    assert_eq!(seen, 1);
    assert!(error.to_string().contains("sink stopped"));
}
