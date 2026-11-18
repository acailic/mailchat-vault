//! Search/export parity through the public v2 API, including source ids
//! whose display labels collide and filters absent from the legacy API,
//! plus the HTML bundle's Merkle inclusion-proof determinism and proof
//! size (P3·20).

use std::path::PathBuf;

use vault_core::export::{export_search, export_search_v2, ExportFormat, ExportSummary};
use vault_core::export_html::{
    export_html, verify_bundle_full, verify_bundle_message, HtmlExportOutcome, HtmlExportScope,
};
use vault_core::message::{Message, NewMessage};
use vault_core::object_store::ObjectStore;
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
            cursor: None,
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
                cursor: None,
                source_ids: sources.clone(),
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
                cursor: None,
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

    // The portable identity: both instances export under the same
    // display label but distinct, label-independent machine keys — the
    // pair a JSON Lines importer dedups on instead of (label,
    // external_id), which would collapse these two rows into one.
    let (_, exported) = export_json(
        &storage,
        &SearchArchiveRequest {
            cursor: None,
            source_ids: sources.clone(),
            ..Default::default()
        },
    );
    assert_eq!(exported.len(), 2);
    let mut keys: Vec<&str> = exported.iter().map(|m| m.source_key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["instance:imap:account-a", "instance:imap:account-b"],
        "connector-owned instances key on kind + connector key"
    );
    let labels: Vec<&str> = exported.iter().map(|m| m.source.as_str()).collect();
    assert_eq!(labels, vec!["Shared mailbox", "Shared mailbox"]);
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
        // Distinct bodies per row: this test drives filters, and the
        // P2.14 near-duplicate fold would otherwise collapse the
        // byte-identical rows into one representative, hiding the
        // author lookup below.
        let mut m = message("mbox", id, date, &format!("contract accepted {id}"));
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
            cursor: None,
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
        cursor: None,
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
        cursor: None,
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
                cursor: None,
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
        cursor: None,
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

/// The P1 regression end to end: two canonical (connector-owned) source
/// instances share a display label and a mail `Message-ID`. Before the
/// portable source identity, the JSON Lines rows both carried
/// `source: "Shared mailbox"` and the importer deduped them onto one
/// `(label, external_id)` identity — a successful subset transfer that
/// silently dropped one instance's message. With `source_key` the
/// subset round-trips through the production bounded importer
/// (`chat_export::import_bounded`) with both bodies retained, stays
/// idempotent, and keeps identity stable across label renames.
#[test]
fn same_label_instances_round_trip_through_the_bounded_importer() {
    let storage = Storage::open_in_memory().unwrap();
    let mut sources = Vec::new();
    for (index, key) in ["account-a", "account-b"].iter().enumerate() {
        let source = storage
            .ensure_source_instance(&NewSourceInstance {
                kind: "imap",
                label: "Shared mailbox",
                connector_key: key,
                config_json: "{}",
            })
            .unwrap();
        let m = message(
            "imap",
            "same-message-id",
            "2026-01-01T12:00:00Z",
            &format!("body from {key}"),
        );
        storage
            .insert_mail_message(
                &source,
                &m,
                "Subject",
                &ConnectorIdentity {
                    namespace: "imap",
                    external_key: "same-message-id",
                    revision_hash: &format!("revision-{index}"),
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

    // Export both instances as one JSON Lines subset.
    let (summary, exported) = export_json(
        &storage,
        &SearchArchiveRequest {
            cursor: None,
            source_ids: sources.clone(),
            ..Default::default()
        },
    );
    assert_eq!(summary.written, 2);
    let mut keys: Vec<&str> = exported.iter().map(|m| m.source_key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["instance:imap:account-a", "instance:imap:account-b"]
    );

    // The file as a picked import, into a fresh vault, through the
    // production bounded path.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subset.jsonl");
    let mut out = Vec::new();
    let summary = export_search_v2(
        &storage,
        &SearchArchiveRequest {
            cursor: None,
            source_ids: sources.clone(),
            ..Default::default()
        },
        ExportFormat::Json,
        &mut out,
    )
    .unwrap();
    assert_eq!(summary.written, 2);
    std::fs::write(&path, &out).unwrap();

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
    assert_eq!(
        (
            outcome.summary.inserted,
            outcome.summary.skipped,
            outcome.summary.failed
        ),
        (2, 0, 0),
        "both same-labelled rows insert; none collapses onto the other"
    );
    assert_eq!(target.stats().unwrap().total, 2);

    // Both bodies are present, each under its own destination instance,
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

    // Re-importing the same file inserts nothing and skips both rows.
    let again = vault_core::chat_export::import_bounded(
        &mut target,
        &path,
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
        (0, 2, 0)
    );
    assert_eq!(target.stats().unwrap().total, 2);

    // Re-exporting from the destination emits the same keys (its
    // instances are legacy-style under those namespaces): the cycle is
    // stable — a further import would land on the same identities.
    let (_, reexported) = export_json(&target, &SearchArchiveRequest::default());
    let mut rekeys: Vec<&str> = reexported.iter().map(|m| m.source_key.as_str()).collect();
    rekeys.sort_unstable();
    assert_eq!(
        rekeys,
        vec!["instance:imap:account-a", "instance:imap:account-b"]
    );

    // Renaming an instance's display label never changes its identity:
    // re-registering the connector with a new label and re-exporting
    // yields the same keys.
    storage
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Renamed mailbox",
            connector_key: "account-a",
            config_json: "{}",
        })
        .unwrap();
    let (_, renamed) = export_json(
        &storage,
        &SearchArchiveRequest {
            cursor: None,
            source_ids: sources.clone(),
            ..Default::default()
        },
    );
    let mut renamed_keys: Vec<&str> = renamed.iter().map(|m| m.source_key.as_str()).collect();
    renamed_keys.sort_unstable();
    assert_eq!(
        renamed_keys,
        vec!["instance:imap:account-a", "instance:imap:account-b"]
    );
    // The display labels followed the rename; the keys did not move.
    let mut renamed_labels: Vec<&str> = renamed.iter().map(|m| m.source.as_str()).collect();
    renamed_labels.sort();
    assert_eq!(renamed_labels, vec!["Renamed mailbox", "Shared mailbox"]);
}

// ----------------------------------------------------------------------
// Honesty filters: export parity — issue #108 reconciliation
// ----------------------------------------------------------------------

/// Seed the same mixed honesty fixture the search-page suite uses (time
/// confidence, grouping, media preservation; shapes copied from the lib
/// tri-state tests in `search2.rs`) and return the storage plus each
/// row's id by external id.
fn honesty_seeded() -> (Storage, std::collections::BTreeMap<&'static str, i64>) {
    let storage = Storage::open_in_memory().unwrap();
    let mut ids = std::collections::BTreeMap::new();
    let mut insert = |key: &'static str, day: u32, tune: &dyn Fn(&mut NewMessage)| {
        let date = if day == 0 {
            String::new()
        } else {
            format!("2026-01-{day:02}T10:00:00Z")
        };
        let mut m = message("demo", key, &date, &format!("needle body {key}"));
        tune(&mut m);
        match storage.insert_message(&m).unwrap() {
            vault_core::message::InsertOutcome::Inserted(id) => {
                ids.insert(key, id);
            }
            _ => panic!("fixture row {key} must insert"),
        }
    };

    // Time: exact, confirmed, derived legacy_assumed, derived missing.
    insert("exact", 1, &|_| {});
    insert("confirmed", 2, &|m| {
        m.time_confidence = "user_confirmed".into()
    });
    insert("assumed", 3, &|m| m.time_confidence = String::new());
    insert("notime", 0, &|_| {});
    // Grouping: verified provider key, legacy chat-name grouping, standalone.
    insert("verified-group", 4, &|m| {
        m.channel = "chat".into();
        m.chat_id = "team".into();
        m.conversation_key = "telegram:chat:9".into();
    });
    insert("legacy-group", 5, &|m| {
        m.channel = "chat".into();
        m.chat_id = "oldgroup".into();
    });
    insert("standalone", 6, &|_| {});
    // Files: stored media, recorded-missing media, WhatsApp placeholder, plain.
    let stored_json = serde_json::json!({
        "media": [{
            "sha256": "h",
            "path": "media/h.jpg",
            "name": "h.jpg",
            "kind": "image",
            "size": 10
        }]
    });
    insert("stored", 7, &|m| m.raw_json = stored_json.to_string());
    let gone_json = serde_json::json!({
        "media": [{
            "name": "gone.jpg",
            "kind": "image",
            "missing": true,
            "reason": "not-in-export"
        }]
    });
    insert("gone", 8, &|m| m.raw_json = gone_json.to_string());
    insert("placeholder", 9, &|m| {
        m.raw_json = r#"{"attachment":{"kind":"image","name":"lost.jpg"}}"#.into();
    });
    insert("plain", 10, &|_| {});
    (storage, ids)
}

/// Apply one honesty filter setting to a request, leaving everything else.
fn with_honesty_filter(
    mut request: SearchArchiveRequest,
    label: &str,
    setting: Option<bool>,
) -> SearchArchiveRequest {
    match label {
        "time_assumed" => request.time_assumed = setting,
        "grouping_unverified" => request.grouping_unverified = setting,
        "files_missing" => request.files_missing = setting,
        other => unreachable!("unknown filter {other}"),
    }
    request
}

#[test]
fn honesty_filters_export_parity_matches_search_hits_and_totals() {
    let (storage, row_ids) = honesty_seeded();
    let named = |keys: &[&str]| -> Vec<i64> {
        let mut ids: Vec<i64> = keys.iter().map(|k| row_ids[*k]).collect();
        ids.sort();
        ids
    };

    for (label, true_names, false_names) in [
        (
            "time_assumed",
            vec!["assumed", "notime"],
            vec![
                "exact",
                "confirmed",
                "verified-group",
                "legacy-group",
                "standalone",
                "stored",
                "gone",
                "placeholder",
                "plain",
            ],
        ),
        (
            "grouping_unverified",
            vec![
                "exact",
                "confirmed",
                "assumed",
                "notime",
                "legacy-group",
                "standalone",
                "stored",
                "gone",
                "placeholder",
                "plain",
            ],
            vec!["verified-group"],
        ),
        (
            "files_missing",
            vec!["gone", "placeholder"],
            vec![
                "exact",
                "confirmed",
                "assumed",
                "notime",
                "verified-group",
                "legacy-group",
                "standalone",
                "stored",
                "plain",
            ],
        ),
    ] {
        for (setting, expected_names) in [(Some(true), true_names), (Some(false), false_names)] {
            let expected = named(&expected_names);

            // A text query one page short of the whole set: the page
            // truncates while its total keeps the filtered count.
            let paged = with_honesty_filter(
                SearchArchiveRequest {
                    text: "needle".into(),
                    limit: Some(1),
                    ..Default::default()
                },
                label,
                setting,
            );
            let page = storage.search_archive(&paged).unwrap();
            assert_eq!(page.hits.len(), 1, "{label}={setting:?}: truncated page");
            assert_eq!(
                page.total,
                expected.len() as u64,
                "{label}={setting:?}: page.total is the filtered count"
            );

            // Exporting the same request writes exactly the search hits
            // and reports the same total.
            let (summary, exported) = export_json(&storage, &paged);
            assert_eq!(
                summary.written, page.total,
                "{label}={setting:?}: summary.written equals page.total"
            );
            let mut exported_ids: Vec<i64> = exported.iter().map(|m| m.id).collect();
            exported_ids.sort();
            assert_eq!(
                exported_ids, expected,
                "{label}={setting:?}: export writes exactly the filtered hit set"
            );

            // The full page of the same scope returns the same set back.
            let full = with_honesty_filter(
                SearchArchiveRequest {
                    text: "needle".into(),
                    ..Default::default()
                },
                label,
                setting,
            );
            let full_page = storage.search_archive(&full).unwrap();
            let mut page_ids: Vec<i64> = full_page.hits.iter().map(|h| h.id).collect();
            page_ids.sort();
            assert_eq!(page_ids, expected, "{label}={setting:?}: full page agrees");
        }
    }
}

// ----------------------------------------------------------------------
// eDiscovery evidence pack (P2·10): Bates-numbered, print-ready
// bundles on top of the A8 HTML export machinery
// ----------------------------------------------------------------------

use vault_core::export_html::{
    export_html, BatesOptions, HtmlExportOutcome, HtmlExportScope, DIGEST_COMMITMENT_LABEL,
    FIDELITY_LABEL, PENDING_PROOF_LINE,
};
use vault_core::object_store::{Capacity, ObjectStore};
use vault_core::storage::{SourceReceiptKind, SourceReceiptRecord};

/// One stored media object in `data_dir` and the `raw_json` media
/// entry the archive links it with.
fn pack_media_entry(data_dir: &std::path::Path, name: &str) -> serde_json::Value {
    let src = data_dir.join("incoming");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join(name), b"evidence bytes").unwrap();
    let (entry, _) = vault_core::media::store(
        data_dir,
        &src.join(name),
        name,
        "document",
        &mut Capacity::default(),
    )
    .unwrap();
    serde_json::json!({
        "sha256": entry.sha256,
        "path": entry.path,
        "name": entry.name,
        "kind": entry.kind,
        "size": entry.size,
    })
}

/// Seed the evidence-pack fixture: March 2026 rows from Ana and Bojan
/// (one with a preserved attachment, one with a missing one), an
/// April row outside the range, an undated row (inside the source and
/// people scope, but outside any date range — date-bounded packs
/// honestly exclude it), one receipt per source, and a second
/// (unscoped) source that must not leak.
/// Returns the storage, the scoped request, and the data_dir.
fn evidence_fixture(dir: &std::path::Path) -> (Storage, SearchArchiveRequest) {
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let stored = pack_media_entry(&data_dir, "contract.pdf");

    let mut ana_attach = message("mbox", "m1", "2026-03-05T09:00:00Z", "contract draft one");
    ana_attach.raw_json = serde_json::json!({ "media": [stored] }).to_string();
    let mut bojan_attach = message("mbox", "m2", "2026-03-12T09:00:00Z", "counterdraft two");
    bojan_attach.author = "Bojan".into();
    bojan_attach.raw_json =
        r#"{"media":[{"name":"notes.txt","kind":"text","missing":true,"reason":"not-in-export"}]}"#
            .into();
    let ana_plain = message("mbox", "m3", "2026-03-20T09:00:00Z", "signed contract");
    let mut april = message("mbox", "m4", "2026-04-02T09:00:00Z", "outside the range");
    april.author = "Vlad".into();
    let mut undated = message("mbox", "m5", "", "no timestamp recorded");
    undated.author = "Ana".into();
    let mut other_source = message("telegram", "t1", "2026-03-08T09:00:00Z", "different source");
    other_source.author = "Ana".into();
    let storage = Storage::open_in_memory().unwrap();
    for m in [
        ana_attach,
        bojan_attach,
        ana_plain,
        april,
        undated,
        other_source,
    ] {
        storage.insert_message(&m).unwrap();
    }

    let mbox = storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.label == "mbox")
        .unwrap();
    let telegram = storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.label == "telegram")
        .unwrap();
    // Receipts for both sources; only the scoped one may appear in
    // the custody appendix.
    for (instance, kind) in [
        (&mbox, SourceReceiptKind::Import),
        (&telegram, SourceReceiptKind::Sync),
    ] {
        storage
            .record_source_receipt(&SourceReceiptRecord {
                source_instance_id: &instance.id,
                kind,
                observed_min: Some("2026-03-01T00:00:00Z"),
                observed_max: Some("2026-03-31T23:59:59Z"),
                new_records: 3,
                present_records: 1,
                failed_records: 0,
                files_preserved: 1,
                files_missing: 1,
                facts_json: "{}",
            })
            .unwrap();
    }

    let request = SearchArchiveRequest {
        cursor: None,
        source_ids: vec![mbox.id],
        author: "ana".into(),
        from_date: Some("2026-03-01".into()),
        to_date: Some("2026-03-31".into()),
        timezone: "UTC".into(),
        ..Default::default()
    };
    (storage, request)
}

/// Run an evidence-pack export into `dest` and unwrap the summary.
fn pack_export_ok(
    s: &Storage,
    request: &SearchArchiveRequest,
    bates: &BatesOptions,
    dest: &std::path::Path,
    data_dir: &std::path::Path,
) -> vault_core::export_html::HtmlExportSummary {
    let objects = ObjectStore::new(data_dir);
    let scope = HtmlExportScope::EvidencePack {
        request: request.clone(),
        bates: bates.clone(),
    };
    match export_html(s, &scope, dest, &objects, &|| false).unwrap() {
        Some(HtmlExportOutcome::Complete(summary)) => summary,
        other => panic!("expected a completed evidence pack, got {other:?}"),
    }
}

/// Every file under `dir`, recursively, as sorted (relative path, bytes).
fn pack_files(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((
                    path.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The Bates footer strings of every page, in page order.
fn pack_bates_footers(dest: &std::path::Path, pages: u64) -> Vec<String> {
    let mut out = Vec::new();
    for page in 1..=pages {
        let html = std::fs::read_to_string(dest.join(format!("page-{page:04}.html"))).unwrap();
        for line in html.lines() {
            if let Some(number) = line
                .strip_prefix("<footer class=\"bates\">")
                .and_then(|l| l.strip_suffix("</footer>"))
            {
                out.push(number.to_string());
            }
        }
    }
    out
}

/// Re-exporting the same (selection, prefix, start) is byte-identical,
/// Bates numbers are sequential and stable, and a different start
/// shifts the whole sequence.
#[test]
fn evidence_pack_reexport_is_byte_identical_and_bates_sequential() {
    let dir = tempfile::tempdir().unwrap();
    let (s, request) = evidence_fixture(dir.path());
    let bates = BatesOptions {
        prefix: "VATL".into(),
        start: 1,
    };

    let first = pack_export_ok(
        &s,
        &request,
        &bates,
        &dir.path().join("pack-a"),
        &dir.path().join("data"),
    );
    let second = pack_export_ok(
        &s,
        &request,
        &bates,
        &dir.path().join("pack-b"),
        &dir.path().join("data"),
    );

    assert_eq!(first, second, "identical input, identical summary");
    assert_eq!(
        pack_files(&dir.path().join("pack-a")),
        pack_files(&dir.path().join("pack-b")),
        "re-exporting the same selection, prefix, and start reproduces the bundle byte for byte"
    );

    // Two scoped rows survive the date range (Ana, March, mbox): the
    // attachment row and the plain row — Bojan's counterdraft is
    // dropped by the people filter, April sits outside the range, and
    // the undated row honestly falls outside any date bound. Bates
    // numbers are sequential across the bundle, oldest first.
    assert_eq!(first.written, 2);
    let footers = pack_bates_footers(&dir.path().join("pack-a"), first.pages);
    assert_eq!(footers, vec!["VATL-000001", "VATL-000002"]);
    assert_eq!(
        first.bates_range.as_deref(),
        Some("VATL-000001 to VATL-000002")
    );

    // Oldest-first: the March 5 draft carries the first number.
    let page1 = std::fs::read_to_string(dir.path().join("pack-a/page-0001.html")).unwrap();
    let draft = page1.find("contract draft one").unwrap();
    let signed = page1.find("signed contract").unwrap();
    assert!(draft < signed, "dated ascending");
    assert!(!page1.contains("no timestamp recorded"));
    let b1 = page1.find("VATL-000001").unwrap();
    let b2 = page1.find("VATL-000002").unwrap();
    assert!(draft < b1 && signed < b2, "the footer follows its message");

    // A different start shifts every number; the bytes change with it.
    let shifted = BatesOptions {
        prefix: "VATL".into(),
        start: 100,
    };
    let third = pack_export_ok(
        &s,
        &request,
        &shifted,
        &dir.path().join("pack-c"),
        &dir.path().join("data"),
    );
    assert_eq!(third.written, 2);
    assert_eq!(
        pack_bates_footers(&dir.path().join("pack-c"), third.pages),
        vec!["VATL-000100", "VATL-000101"]
    );
}

/// The cover sheet states the exact scope, its counts match the
/// exported rows, the commitment is labeled a digest commitment (and
/// never anything stronger), the pending line is present, pages carry
/// print CSS and headers, and the custody appendix shows only the
/// scoped sources' receipts with verification instructions.
#[test]
fn evidence_pack_cover_counts_labels_and_custody_appendix() {
    let dir = tempfile::tempdir().unwrap();
    let (s, request) = evidence_fixture(dir.path());
    let dest = dir.path().join("pack");
    let summary = pack_export_ok(
        &s,
        &request,
        &BatesOptions {
            prefix: "VATL".into(),
            start: 1,
        },
        &dest,
        &dir.path().join("data"),
    );

    // The scoped rows are exactly Ana's March mbox rows: m1 (stored
    // attachment), m3 (plain), m5 (undated) — Bojan's m2 carries an
    // attachment but the people filter drops it, April and the
    // telegram source are out of scope.
    let page1 = std::fs::read_to_string(dest.join("page-0001.html")).unwrap();
    let section_count = page1.matches("<section class=\"message\"").count() as u64;
    let attachment_lis = page1.matches("<li>").count() as u64;

    let cover = std::fs::read_to_string(dest.join("index.html")).unwrap();
    // Scope statement: date range, source label, people filter.
    assert!(cover.contains("date range 2026-03-01 to 2026-03-31"));
    assert!(cover.contains("sources: mbox"));
    assert!(cover.contains("people: ana"));
    // Counts match the actual exported rows — not the planned count.
    assert!(cover.contains(&format!("messages: {}", summary.written)));
    assert_eq!(
        summary.written, section_count,
        "cover count equals exported rows"
    );
    assert!(cover.contains(&format!(
        "attachment references: {}",
        summary.attachment_refs.unwrap()
    )));
    assert_eq!(summary.attachment_refs, Some(attachment_lis));
    // The commitment is labeled exactly a digest commitment and the
    // pending line states cryptographic proof is outstanding; nothing
    // claims a stronger proof.
    assert!(cover.contains(DIGEST_COMMITMENT_LABEL));
    assert!(cover.contains(PENDING_PROOF_LINE));
    assert!(!cover.to_lowercase().contains("merkle"));
    assert!(cover.contains(FIDELITY_LABEL));
    assert!(cover.contains("Bates numbers: VATL-000001 to VATL-000002"));
    assert!(cover.contains("href=\"custody.html\""));
    // The commitment on the cover is the manifest's commitment.
    let commitment = summary.digest_commitment.unwrap();
    assert_eq!(commitment.len(), 64);
    assert!(cover.contains(&commitment));
    let manifest = std::fs::read_to_string(dest.join("manifest")).unwrap();
    assert!(manifest.contains(&format!(
        "{DIGEST_COMMITMENT_LABEL} (SHA-256 over the ordered per-message digests): {commitment}"
    )));
    assert!(manifest.matches(" digest=").count() == section_count as usize);

    // Message pages: print CSS, Bates footers, headers, and the
    // attachment digest.
    assert!(page1.contains("@media print"));
    assert!(page1.contains("@page"));
    assert!(page1.contains("footer.bates"));
    assert!(page1.contains("<footer class=\"bates\">VATL-000001</footer>"));
    assert!(page1.contains("<th>From</th>"));
    assert!(page1.contains("<th>To</th>"));
    assert!(page1.contains("<th>Date</th>"));
    assert!(page1.contains("<th>Source</th>"));
    assert!(page1.contains("sha256 "));

    // Chain of custody: only the scoped source's receipts, plus the
    // release-checksum verification pointer.
    let custody = std::fs::read_to_string(dest.join("custody.html")).unwrap();
    assert!(custody.contains("Chain of custody"));
    assert!(custody.contains("import"));
    assert!(custody.contains("2026-03-01T00:00:00Z to 2026-03-31T23:59:59Z"));
    assert!(custody.contains("docs/release.md"));
    assert!(custody.contains("release-verify.sh"));
    assert!(!custody.contains("different source"));

    // Offline rules still hold across the whole bundle.
    for (path, bytes) in pack_files(&dest) {
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        assert!(!text.contains("<script"), "{}: no scripts", path.display());
        assert!(
            !text.contains("http://") && !text.contains("https://"),
            "{}: nothing remote",
            path.display()
        );
    }
}

/// The plan step answers scope and count for the evidence-pack scope
/// before anything is written, from the same normalized filters — and
/// a pack without date bounds scopes to the undated row too, numbered
/// last behind the dated ones.
#[test]
fn evidence_pack_plan_states_scope_and_count() {
    let dir = tempfile::tempdir().unwrap();
    let (s, request) = evidence_fixture(dir.path());
    let bates = || BatesOptions {
        prefix: "VATL".into(),
        start: 1,
    };
    let plan = vault_core::export_html::plan_html_export(
        &s,
        &HtmlExportScope::EvidencePack {
            request: request.clone(),
            bates: bates(),
        },
    )
    .unwrap();
    assert_eq!(plan.scope_label, "evidence pack");
    assert_eq!(plan.count, 2, "the date range excludes the undated row");

    let unbounded = SearchArchiveRequest {
        from_date: None,
        to_date: None,
        ..request.clone()
    };
    let summary = pack_export_ok(
        &s,
        &unbounded,
        &bates(),
        &dir.path().join("unbounded"),
        &dir.path().join("data"),
    );
    assert_eq!(
        summary.written, 3,
        "no date bounds: the undated row rides along"
    );
    let page1 = std::fs::read_to_string(dir.path().join("unbounded/page-0001.html")).unwrap();
    let draft = page1.find("contract draft one").unwrap();
    let signed = page1.find("signed contract").unwrap();
    let undated = page1.find("no timestamp recorded").unwrap();
    assert!(
        draft < signed && signed < undated,
        "dated ascending, undated last"
    );
    assert_eq!(
        pack_bates_footers(&dir.path().join("unbounded"), summary.pages),
        vec!["VATL-000001", "VATL-000002", "VATL-000003"]
    );
}

// ----------------------------------------------------------------------
// HTML bundle Merkle inclusion proofs (P3·20)
// ----------------------------------------------------------------------

/// Write the whole archive as an HTML export bundle into `dir/name` and
/// return its path.
fn write_html_bundle(storage: &Storage, dir: &std::path::Path, name: &str) -> PathBuf {
    let objects = ObjectStore::new(dir);
    let dest = dir.join(name);
    match export_html(
        storage,
        &HtmlExportScope::Search {
            request: SearchArchiveRequest::default(),
        },
        &dest,
        &objects,
        &|| false,
    )
    .unwrap()
    .unwrap()
    {
        HtmlExportOutcome::Complete(_) => dest,
        HtmlExportOutcome::Cancelled => panic!("uncancelled bundle must complete"),
    }
}

fn merkle_seeded(ids: [&'static str; 4]) -> Storage {
    let storage = Storage::open_in_memory().unwrap();
    for (i, id) in ids.iter().enumerate() {
        storage
            .insert_message(&message(
                "mbox",
                id,
                &format!("2026-01-0{}T10:00:00Z", i + 1),
                &format!("body of {id}"),
            ))
            .unwrap();
    }
    storage
}

/// Determinism: two exports of the same data carry the same Merkle
/// root, the full-root check verifies, one cited message verifies
/// against that root without the rest, and the bundle's README states
/// the non-guarantees plainly.
#[test]
fn html_bundle_merkle_root_is_deterministic_across_two_exports() {
    let storage = merkle_seeded(["m4", "m2", "m3", "m1"]);
    let dir_one = tempfile::tempdir().unwrap();
    let dir_two = tempfile::tempdir().unwrap();
    let one = write_html_bundle(&storage, dir_one.path(), "bundle-one");
    let two = write_html_bundle(&storage, dir_two.path(), "bundle-two");

    let check_one = verify_bundle_full(&one).unwrap();
    let check_two = verify_bundle_full(&two).unwrap();
    assert!(check_one.valid, "first bundle's root recomputes");
    assert!(check_two.valid, "second bundle's root recomputes");
    assert_eq!(check_one.messages, 4);
    assert_eq!(
        check_one.stated_root, check_two.stated_root,
        "same data, same root across two exports"
    );
    assert_eq!(check_one.stated_root, check_one.recomputed_root);

    // The single-message check: one cited message, verified against the
    // root without opening the rest of the bundle.
    let single = verify_bundle_message(&one, "m2")
        .unwrap()
        .expect("m2 is listed in the manifest");
    assert!(single.path_valid);
    assert_eq!(single.proved_root, check_one.stated_root.unwrap());
    assert_eq!(single.bundle_root, check_two.stated_root);
    assert!(verify_bundle_message(&one, "no-such-id").unwrap().is_none());

    // The guarantee statement travels with the bundle.
    let readme = std::fs::read_to_string(one.join("README")).unwrap();
    assert!(readme.contains("NOT third-party notarization"));
}

/// Proof-size sanity: with four messages each recorded proof path holds
/// at most log2(4) = 2 hashes (the wire form of a path is
/// comma-separated `L<hex>`/`R<hex>` sibling hashes).
#[test]
fn html_bundle_proof_paths_hold_at_most_log2_hashes() {
    let storage = merkle_seeded(["m1", "m2", "m3", "m4"]);
    let dir = tempfile::tempdir().unwrap();
    let bundle = write_html_bundle(&storage, dir.path(), "bundle");
    let manifest = std::fs::read_to_string(bundle.join("manifest")).unwrap();

    let mut proof_lines = 0;
    for line in manifest.lines().filter(|l| l.starts_with("proof:")) {
        proof_lines += 1;
        // proof:<digest>:<steps>:<external-id>
        let steps = line.split(':').nth(2).unwrap();
        let hashes = if steps.is_empty() {
            0
        } else {
            steps.split(',').count()
        };
        assert!(
            hashes <= 2,
            "n=4: each proof path holds at most 2 hashes, got {hashes}"
        );
        for step in steps.split(',').filter(|s| !s.is_empty()) {
            assert_eq!(step.len(), 65, "one side marker + 64 hex chars");
            assert!(step.starts_with('L') || step.starts_with('R'));
        }
    }
    assert_eq!(proof_lines, 4, "one proof line per exported message");
}
