use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

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
    assert_eq!(s.schema_version().unwrap(), 6);

    // Reopening the same file applies nothing new and keeps the data.
    let s2 = Storage::open(&db).unwrap();
    assert_eq!(s2.schema_version().unwrap(), 6);
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
        .search_filtered("\"vault\"", Some("2026-10-1"), Some("2026-10-9"), 10)
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|m| m.external_id.as_str()).collect();
    assert_eq!(ids, ["2", "3", "4"]);

    // Padded bounds keep working unchanged.
    let found = s
        .search_filtered("\"vault\"", Some("2026-10-01"), None, 10)
        .unwrap();
    assert_eq!(found.len(), 4);

    // Non-padded spelling of an invalid date is still an error.
    assert!(s
        .search_filtered("\"vault\"", Some("2026-13-1"), None, 10)
        .is_err());

    // Wide signed years are refused instead of panicking or silently
    // mis-ordering against 4-digit sent_at text.
    assert!(s
        .search_filtered("\"vault\"", Some("+262143-12-31"), None, 10)
        .is_err());
    assert!(s
        .search_filtered("\"vault\"", None, Some("+262143-12-31"), 10)
        .is_err());
    assert!(
        s.search_filtered("\"vault\"", None, Some("9999-12-31"), 10)
            .is_err(),
        "date_to whose successor leaves the 4-digit range is refused"
    );
    // The last upper bound with a representable successor still works.
    assert!(s
        .search_filtered("\"vault\"", None, Some("9999-12-30"), 10)
        .is_ok());
}
