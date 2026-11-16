use vault_core::llm::{run_plan, validate, PlanSource, QueryPlan};
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::search::build_match_query;
use vault_core::storage::{
    ConnectorIdentity, MailInsertOutcome, NewSourceInstance, Occurrence, Storage,
    SUPPORTED_SCHEMA_VERSION,
};

fn msg(external_id: &str, author: &str, body: &str) -> NewMessage {
    NewMessage::new("test", external_id, author, "2026-01-01T00:00:00Z", body)
}

fn storage() -> Storage {
    Storage::open_in_memory().expect("open in-memory storage")
}

fn search(s: &Storage, text: &str, limit: u32) -> vault_core::query::SearchPage {
    s.search_page(&SearchQuery {
        text: text.into(),
        limit,
        ..SearchQuery::default()
    })
    .unwrap()
}

#[test]
fn migrations_apply_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), SUPPORTED_SCHEMA_VERSION);

    // Reopening the same file applies nothing new and keeps the data.
    let s2 = Storage::open(&db).unwrap();
    assert_eq!(s2.schema_version().unwrap(), SUPPORTED_SCHEMA_VERSION);
    s.insert_message(&msg("a", "x", "hello vault")).unwrap();
    assert_eq!(s2.stats().unwrap().total, 1);
}

#[test]
fn insert_is_idempotent_on_source_and_external_id() {
    let s = storage();
    let first = s.insert_message(&msg("m1", "ana", "first body")).unwrap();
    assert!(matches!(first, InsertOutcome::Inserted(_)));

    // Same dedup key, different content: no error, no second row, original
    // body untouched.
    let second = s
        .insert_message(&msg("m1", "ana", "changed body that must not be stored"))
        .unwrap();
    assert!(
        matches!(second, InsertOutcome::Duplicate(id) if id == match first {
            InsertOutcome::Inserted(id) => id,
            _ => unreachable!(),
        })
    );

    // Same external_id under a different source namespace is a new message.
    let other_source = NewMessage::new("other", "m1", "ana", "", "hello");
    assert!(matches!(
        s.insert_message(&other_source).unwrap(),
        InsertOutcome::Inserted(_)
    ));

    assert_eq!(s.stats().unwrap().total, 2);
    let found = search(&s, "changed", 10).hits;
    assert!(found.is_empty(), "changed body must not be searchable");
}

#[test]
fn fts_search_matches_body_and_author() {
    let s = storage();
    s.insert_message(&msg(
        "1",
        "ana <ana@example.com>",
        "quarterly report attached",
    ))
    .unwrap();
    s.insert_message(&msg("2", "marko", "nothing relevant here"))
        .unwrap();

    let body_hits = search(&s, "quarterly", 10).hits;
    assert_eq!(body_hits.len(), 1);
    assert_eq!(body_hits[0].message.external_id, "1");

    let author_hits = search(&s, "marko", 10).hits;
    assert_eq!(author_hits.len(), 1);
    assert_eq!(author_hits[0].message.external_id, "2");
}

#[test]
fn fts_search_respects_limit() {
    let s = storage();
    for i in 0..5 {
        s.insert_message(&msg(&format!("m{i}"), "a", "vault vault vault"))
            .unwrap();
    }
    assert_eq!(search(&s, "vault", 3).hits.len(), 3);
}

#[test]
fn fts_search_orders_by_relevance() {
    let s = storage();
    s.insert_message(&msg("dense", "a", "vault vault vault"))
        .unwrap();
    s.insert_message(&msg(
        "sparse",
        "b",
        &format!("{} vault {}", "filler ".repeat(40), "tail".repeat(40)),
    ))
    .unwrap();
    let hits = search(&s, "vault", 10).hits;
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].message.external_id, "dense",
        "denser document should rank first"
    );
}

/// Insert one subject-bearing mail row through the connector path — the
/// only ingest that populates the FTS `subject` column. `body` is the
/// caller's composition (production mail stores `"{subject}\n\n{text}"`).
fn insert_mail(
    s: &Storage,
    external_id: &str,
    author: &str,
    sent_at: &str,
    subject: &str,
    body: &str,
) -> i64 {
    let instance = s
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Subject test mail",
            connector_key: "imap|subject.test|993|implicit-tls|subj@example.test",
            config_json: "{}",
        })
        .unwrap();
    let message = NewMessage::new(instance.clone(), external_id, author, sent_at, body);
    let identity = ConnectorIdentity {
        namespace: "imap",
        external_key: external_id,
        revision_hash: "subject-test-1",
    };
    let occurrence = Occurrence {
        scope: "INBOX",
        provider_item_id: external_id,
        uidvalidity: Some(1),
    };
    match s
        .insert_mail_message(&instance, &message, subject, &identity, &occurrence)
        .unwrap()
    {
        MailInsertOutcome::Inserted { id, .. } | MailInsertOutcome::Duplicate { id } => id,
    }
}

#[test]
fn subject_column_filter_flows_through_the_ask_plan() {
    let s = storage();
    let subject_id = insert_mail(
        &s,
        "subj-mail",
        "ana",
        "2026-10-01T09:00:00Z",
        "vault migration plan",
        "vault migration plan\n\nstatus update covering the rollout steps, owners, \
         timing, and the open questions that still need answers",
    );
    s.insert_message(&NewMessage::new(
        "test",
        "body-only",
        "bob",
        "2026-10-02T09:00:00Z",
        "notes about the vault and its rotation keys",
    ))
    .unwrap();

    // `subject:` filters compile against archive_fts — the old restriction
    // lived in the translation prompt only.
    assert!(s.is_valid_match("subject: vault"));
    assert!(s.is_valid_match("subject: \"vault migration\""));

    // So the plan an LLM may now produce survives vetting unchanged.
    let plan = QueryPlan {
        match_expr: "subject: vault".into(),
        date_from: None,
        date_to: None,
        from: None,
        has_link: None,
        extract_links: false,
    };
    let vetted = validate(&s, "vault migration plan", Ok(plan));
    assert_eq!(vetted.source, PlanSource::Llm);
    assert!(vetted.notes.is_empty());

    // run_plan returns exactly the subject-bearing message; the body-only
    // row a plain `vault` term also reaches stays out of a subject filter.
    let run: Vec<i64> = run_plan(&s, &vetted, 10)
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(run, vec![subject_id]);

    let filtered: Vec<i64> = s
        .search_filtered("subject: vault", None, None, None, false, 10)
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(filtered, vec![subject_id]);

    let plain: Vec<i64> = s
        .search_filtered("\"vault\"", None, None, None, false, 10)
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(plain.len(), 2, "a plain term still matches both rows");
}

#[test]
fn subject_hit_outranks_denser_body_hit() {
    let s = storage();
    // A: the term lives in the subject line; the body is the production
    // mail composition "{subject}\n\n{text}" with no further mentions.
    let a = insert_mail(
        &s,
        "subject-hit",
        "ana",
        "2026-10-01T09:00:00Z",
        "vault migration plan",
        "vault migration plan\n\nstatus update covering the rollout steps, owners, \
         timing, and the open questions that still need answers",
    );
    // B: no subject term; a longer body mentioning vault twice. Under the
    // old equal-column-weight ranking B won on body evidence; the subject
    // column weight must flip the order.
    let b = match s
        .insert_message(&NewMessage::new(
            "test",
            "body-hit",
            "bob",
            "2026-10-02T09:00:00Z",
            "the vault handles secrets and the vault rotates keys; this body \
             is longer overall and mentions the term twice somewhere inside",
        ))
        .unwrap()
    {
        InsertOutcome::Inserted(id) | InsertOutcome::Duplicate(id) => id,
    };
    // Unrelated filler: with the term in 2 of 8 rows "vault" carries a
    // real idf (FTS5's bm25 zeroes the idf when a term hits half the
    // corpus), so the weighting operates on healthy margins.
    for (n, body) in [
        "lunch plans for friday at the noodle place near the office",
        "the quarterly review slides are attached with revenue and churn numbers",
        "gardening notes: plant the garlic beds before the first frost arrives",
        "boarding pass and hotel confirmation for the conference trip next month",
        "the espresso machine in the kitchen is fixed, enjoy the good coffee again",
        "reminder about the dental appointment on thursday afternoon",
    ]
    .into_iter()
    .enumerate()
    {
        s.insert_message(&NewMessage::new(
            "test",
            format!("filler-{n}"),
            "cid",
            "2026-10-03T09:00:00Z",
            body,
        ))
        .unwrap();
    }

    let paged: Vec<i64> = search(&s, "vault", 10)
        .hits
        .iter()
        .map(|h| h.message.id)
        .collect();
    assert_eq!(paged, vec![a, b], "the subject hit must rank first");

    let filtered: Vec<i64> = s
        .search_filtered("\"vault\"", None, None, None, false, 10)
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(filtered, vec![a, b]);
}

#[test]
fn sanitized_user_queries_never_error() {
    let s = storage();
    s.insert_message(&msg("1", "ana", "vault notes")).unwrap();
    for input in [
        "",
        "   ",
        "vault",
        "user's *weird* (input)",
        "\"unbalanced",
        "-not NEAR *",
        "двонационални тест",
    ] {
        let q = build_match_query(input);
        if q.is_empty() {
            continue;
        }
        s.search_page(&SearchQuery {
            text: input.into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap_or_else(|e| panic!("query {input:?} failed: {e}"));
    }
}

#[test]
fn stats_group_by_source() {
    let s = storage();
    s.insert_message(&msg("1", "a", "one")).unwrap();
    s.insert_message(&msg("2", "a", "two")).unwrap();
    let other = NewMessage::new("mbox", "x", "a", "", "three");
    s.insert_message(&other).unwrap();

    let stats = s.stats().unwrap();
    assert_eq!(stats.total, 3);
    assert_eq!(
        stats.by_source,
        vec![
            vault_core::message::SourceCount {
                source: "test".into(),
                count: 2
            },
            vault_core::message::SourceCount {
                source: "mbox".into(),
                count: 1
            },
        ]
    );
}

#[test]
fn get_returns_stored_message() {
    let s = storage();
    let outcome = s
        .insert_message(&msg("1", "ana <a@b>", "hello body"))
        .unwrap();
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        _ => unreachable!(),
    };
    let m = s.get(id).unwrap().expect("message exists");
    assert_eq!(m.body, "hello body");
    assert_eq!(m.author, "ana <a@b>");
    assert!(s.get(9999).unwrap().is_none());
}

#[test]
fn rebuild_fts_restores_index() {
    let s = storage();
    s.insert_message(&msg("1", "ana", "searchable body"))
        .unwrap();
    // Wipe the index directly to simulate corruption, then rebuild.
    s.wipe_fts_for_tests().unwrap();
    assert!(search(&s, "searchable", 10).hits.is_empty());
    s.rebuild_fts().unwrap();
    assert_eq!(search(&s, "searchable", 10).hits.len(), 1);
}

#[test]
fn date_bounds_are_canonicalized_before_text_comparison() {
    let s = storage();
    let days = [
        ("1", "2026-09-30T12:00:00Z"),
        ("2", "2026-10-01T12:00:00Z"),
        ("3", "2026-10-05T12:00:00Z"),
        ("4", "2026-10-09T12:00:00Z"),
        ("5", "2026-10-10T12:00:00Z"),
    ];
    for (id, sent_at) in days {
        s.insert_message(&NewMessage::new("test", id, "a", sent_at, "vault"))
            .unwrap();
    }

    // Non-padded bounds must behave exactly like their padded spellings.
    let found = s
        .search_filtered(
            "\"vault\"",
            Some("2026-10-1"),
            Some("2026-10-9"),
            None,
            false,
            10,
        )
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|m| m.external_id.as_str()).collect();
    assert_eq!(ids, ["2", "3", "4"]);

    // Padded bounds keep working unchanged.
    let found = s
        .search_filtered("\"vault\"", Some("2026-10-01"), None, None, false, 10)
        .unwrap();
    assert_eq!(found.len(), 4);

    // Non-padded spelling of an invalid date is still an error.
    assert!(s
        .search_filtered("\"vault\"", Some("2026-13-1"), None, None, false, 10)
        .is_err());

    // Wide signed years are refused instead of panicking or silently
    // mis-ordering against 4-digit sent_at text.
    assert!(s
        .search_filtered("\"vault\"", Some("+262143-12-31"), None, None, false, 10)
        .is_err());
    assert!(s
        .search_filtered("\"vault\"", None, Some("+262143-12-31"), None, false, 10)
        .is_err());
    assert!(
        s.search_filtered("\"vault\"", None, Some("9999-12-31"), None, false, 10)
            .is_err(),
        "date_to whose successor leaves the 4-digit range is refused"
    );
    // The last upper bound with a representable successor still works.
    assert!(s
        .search_filtered("\"vault\"", None, Some("9999-12-30"), None, false, 10)
        .is_ok());
}

// ---------------------------------------------------------------------------
// Finished-jobs ledger (migration 16)
// ---------------------------------------------------------------------------

fn ledger_entry(
    n: u64,
    kind: &str,
    state: &str,
    summary: &str,
) -> vault_core::storage::JobLedgerEntry {
    vault_core::storage::JobLedgerEntry {
        job_id: n,
        kind: kind.into(),
        submitted_at: format!("2026-11-0{}/1T10:00:00Z", (n % 9) + 1),
        finished_at: format!("2026-11-0{}/1T10:01:00Z", (n % 9) + 1),
        state: state.into(),
        summary: summary.into(),
        payload: format!("{{\"state\":\"{state}\",\"result\":{{\"inserted\":{n}}}}}"),
    }
}

#[test]
fn job_ledger_roundtrips_newest_first() {
    let s = storage();
    s.record_job_ledger(&ledger_entry(1, "import_mbox", "succeeded", "10 new"))
        .unwrap();
    s.record_job_ledger(&ledger_entry(
        2,
        "imap_sync",
        "failed",
        "connection refused",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(3, "seed_demo", "cancelled", ""))
        .unwrap();

    let rows = s.job_ledger_recent(10).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.job_id).collect::<Vec<_>>(),
        vec![3, 2, 1],
        "newest first"
    );
    assert_eq!(rows[0].state, "cancelled");
    assert_eq!(rows[1].kind, "imap_sync");
    assert_eq!(rows[1].summary, "connection refused");
    assert!(rows[2].payload.contains("\"inserted\":1"));

    // The limit caps the read, never the stored history.
    assert_eq!(s.job_ledger_recent(2).unwrap().len(), 2);
    assert!(s.job_ledger_recent(0).unwrap().is_empty());

    // The state CHECK keeps the ledger terminal-only.
    assert!(s
        .record_job_ledger(&ledger_entry(4, "x", "running", ""))
        .is_err());
}

#[test]
fn job_ledger_prunes_to_the_newest_fifty_on_write() {
    let s = storage();
    let keep = vault_core::storage::JOB_LEDGER_KEEP;
    for n in 0..(keep + 7) as u64 {
        s.record_job_ledger(&ledger_entry(n, "import_mbox", "succeeded", "n"))
            .unwrap();
    }
    let rows = s.job_ledger_recent(u32::MAX).unwrap();
    assert_eq!(rows.len(), keep, "the ledger stays bounded");
    // The pruned tail is the oldest: survivors are the newest `keep`.
    assert_eq!(rows.first().unwrap().job_id, (keep + 6) as u64);
    assert_eq!(rows.last().unwrap().job_id, 7);
}
