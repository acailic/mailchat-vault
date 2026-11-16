//! Integration tests for paged/filtered search (`search_page`) and chat
//! thread context (`chat_context`).

use vault_core::message::NewMessage;
use vault_core::query::{ChatContext, SearchQuery};
use vault_core::storage::Storage;

fn chat_msg(
    source: &str,
    external_id: &str,
    chat_id: &str,
    author: &str,
    sent_at: &str,
    body: &str,
) -> NewMessage {
    let mut m = NewMessage::new(source, external_id, author, sent_at, body);
    m.chat_id = chat_id.into();
    m
}

/// A small fixture mixing sources, chats, authors, and dates.
fn seeded() -> Storage {
    let s = Storage::open_in_memory().unwrap();
    let msgs = [
        chat_msg(
            "mbox",
            "e1",
            "thread-a",
            "Ana <ana@corp>",
            "2026-01-05T09:00:00Z",
            "quarterly report for the vault migration",
        ),
        chat_msg(
            "mbox",
            "e2",
            "thread-a",
            "Marko <marko@corp>",
            "2026-01-05T10:00:00Z",
            "re: quarterly report looks good",
        ),
        chat_msg(
            "mbox",
            "e3",
            "thread-b",
            "Jelena <jelena@corp>",
            "2026-02-10T08:30:00Z",
            "lunch plans unrelated",
        ),
        chat_msg(
            "whatsapp",
            "w1",
            "family",
            "mom",
            "2026-01-20T12:00:00Z",
            "call me about the vault keys",
        ),
        chat_msg(
            "whatsapp",
            "w2",
            "family",
            "mom",
            "2026-01-21T18:05:00Z",
            "thanks! quarterly taxes done",
        ),
        chat_msg(
            "telegram",
            "t1",
            "devs",
            "nina",
            "2026-03-01T08:00:00Z",
            "shipping the fts search tomorrow",
        ),
        chat_msg(
            "telegram",
            "t2",
            "devs",
            "ognjen",
            "2026-03-01T08:01:00Z",
            "nice, the vault search is fast",
        ),
        chat_msg(
            "telegram",
            "t3",
            "devs",
            "nina",
            "2026-03-01T08:02:00Z",
            "one hundred milliseconds budget",
        ),
    ];
    for m in &msgs {
        s.insert_message(m).unwrap();
    }
    s
}

#[test]
fn text_search_returns_snippets_with_match_marks() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert!(page.total >= 3);
    assert_eq!(page.hits.len() as u64, page.total);
    for hit in &page.hits {
        let snip = hit.snippet.as_deref().expect("fts mode returns a snippet");
        assert!(
            snip.contains('\u{1}') && snip.contains('\u{2}'),
            "snippet must mark matches: {snip:?}"
        );
    }
}

#[test]
fn source_filter_restricts_results() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            sources: vec!["whatsapp".into()],
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert!(page.total > 0);
    assert!(page.hits.iter().all(|h| h.message.source == "whatsapp"));
}

#[test]
fn author_filter_is_case_insensitive_substring() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: "report".into(),
            author: "MARKO".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].message.external_id, "e2");
}

#[test]
fn author_filter_escapes_like_wildcards() {
    let s = seeded();
    // '%' as author filter must not match everything.
    let page = s
        .search_page(&SearchQuery {
            text: "report".into(),
            author: "%".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 0, "'%' must be literal, not a wildcard");
}

#[test]
fn author_filter_folds_unicode_case() {
    let s = seeded();
    let mut m = NewMessage::new(
        "mbox",
        "e4",
        "Ivan Ivić <ivan@corp>",
        "2026-02-11T09:00:00Z",
        "ivić's quarterly report",
    );
    m.chat_id = "thread-b".into();
    s.insert_message(&m).unwrap();

    let hits = |author: &str| -> u64 {
        s.search_page(&SearchQuery {
            text: "report".into(),
            author: author.into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap()
        .total
    };

    assert_eq!(
        hits("ivić"),
        1,
        "lowercase diacritic pattern finds its author"
    );
    assert_eq!(hits("IVIĆ"), 1, "uppercase pattern folds the same");
    assert_eq!(
        hits("Ivan Ivić"),
        1,
        "pattern differing in case and spacing still folds to a match"
    );
    assert_eq!(hits("ivic"), 0, "pattern differing beyond case must miss");
}

#[test]
fn date_range_filters_inclusively() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            from_date: "2026-01-19".into(),
            to_date: "2026-01-21".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    // Only w1 ("call me about the vault keys", Jan 20) has "vault" in range.
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].message.external_id, "w1");
}

#[test]
fn from_date_only_and_to_date_only_both_apply() {
    let s = seeded();
    let from_only = s
        .search_page(&SearchQuery {
            text: "quarterly".into(),
            from_date: "2026-01-06".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    // e1 (Jan 5) excluded; w2 (Jan 21) kept.
    assert_eq!(from_only.total, 1);
    assert_eq!(from_only.hits[0].message.external_id, "w2");

    // e1 and e2 are both on Jan 5, within the to_date bound.
    let to_only = s
        .search_page(&SearchQuery {
            text: "quarterly".into(),
            to_date: "2026-01-06".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(to_only.total, 2);
    let ids: Vec<&str> = to_only
        .hits
        .iter()
        .map(|h| h.message.external_id.as_str())
        .collect();
    assert!(ids.contains(&"e1") && ids.contains(&"e2"));
}

#[test]
fn date_filters_follow_the_local_display_day() {
    let s = Storage::open_in_memory().unwrap();
    // Near-midnight message: which local day it "displays" in depends on
    // the machine offset, so derive the expected day from the same rule
    // (instant + offset) the filter layer must agree with.
    s.insert_message(&chat_msg(
        "mbox",
        "m1",
        "c",
        "a",
        "2026-01-05T23:30:00Z",
        "vault near midnight",
    ))
    .unwrap();

    let t = chrono::NaiveDateTime::parse_from_str("2026-01-05T23:30:00Z", "%Y-%m-%dT%H:%M:%SZ")
        .unwrap();
    // Resolve the displayed local day through the machine timezone the same
    // way the filter layer does (historical offset for the instant).
    let instant_utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(t, chrono::Utc);
    let local_date = instant_utc.with_timezone(&chrono::Local).date_naive();
    let local_day = local_date.to_string();
    let prev_day = (local_date - chrono::Duration::days(1)).to_string();
    let next_day = (local_date + chrono::Duration::days(1)).to_string();

    let count = |from: &str, to: &str| -> u64 {
        s.search_page(&SearchQuery {
            text: "vault".into(),
            from_date: from.into(),
            to_date: to.into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap()
        .total
    };

    // The displayed local day must be exactly the filterable day.
    assert_eq!(count(&local_day, ""), 1, "from = display day includes");
    assert_eq!(count("", &local_day), 1, "to = display day includes");
    assert_eq!(
        count("", &prev_day),
        0,
        "to before the display day excludes"
    );
    assert_eq!(
        count(&next_day, ""),
        0,
        "from after the display day excludes"
    );
}

#[test]
fn invalid_date_fails_the_query() {
    let s = seeded();
    let err = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            from_date: "2026-1-5".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("YYYY-MM-DD"), "{err}");
}

#[test]
fn date_filters_consistently_exclude_empty_sent_at() {
    let s = Storage::open_in_memory().unwrap();
    s.insert_message(&chat_msg(
        "mbox",
        "dated",
        "c",
        "a",
        "2026-01-05T09:00:00Z",
        "vault dated body",
    ))
    .unwrap();
    s.insert_message(&chat_msg(
        "mbox",
        "undated",
        "c",
        "a",
        "",
        "vault undated body",
    ))
    .unwrap();

    // Both single-bound filters exclude the undated message.
    for q in [
        SearchQuery {
            text: "vault".into(),
            from_date: "2026-01-01".into(),
            limit: 10,
            ..SearchQuery::default()
        },
        SearchQuery {
            text: "vault".into(),
            to_date: "2026-01-31".into(),
            limit: 10,
            ..SearchQuery::default()
        },
    ] {
        let page = s.search_page(&q).unwrap();
        assert_eq!(page.total, 1, "undated message must be excluded: {q:?}");
        assert_eq!(page.hits[0].message.external_id, "dated");
    }

    // With no date bound both messages stay searchable.
    let all = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(all.total, 2);
}

#[test]
fn pagination_limits_offsets_and_counts() {
    let s = seeded();
    let q = |limit: u32, offset: u32| SearchQuery {
        text: "vault".into(),
        limit,
        offset,
        ..SearchQuery::default()
    };
    let page1 = s.search_page(&q(2, 0)).unwrap();
    assert_eq!(page1.hits.len(), 2);
    assert!(page1.total > 2);
    assert_eq!(page1.offset, 0);

    let page2 = s.search_page(&q(2, 2)).unwrap();
    // "vault" matches e1, w1, t2 → offset 2 leaves exactly one hit.
    assert_eq!(page2.hits.len(), 1);
    assert_eq!(page2.total, page1.total);

    // No overlap between pages.
    let ids1: Vec<i64> = page1.hits.iter().map(|h| h.message.id).collect();
    let ids2: Vec<i64> = page2.hits.iter().map(|h| h.message.id).collect();
    assert!(ids1.iter().all(|id| !ids2.contains(id)));

    // Offset beyond total returns an empty page.
    let far = s.search_page(&q(2, 1000)).unwrap();
    assert!(far.hits.is_empty());
    assert_eq!(far.total, page1.total);
}

#[test]
fn browse_mode_lists_newest_first_without_snippets() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: String::new(),
            limit: 3,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 8);
    assert_eq!(page.hits.len(), 3);
    // Newest first: telegram t3, t2, t1.
    let ids: Vec<&str> = page
        .hits
        .iter()
        .map(|h| h.message.external_id.as_str())
        .collect();
    assert_eq!(ids, vec!["t3", "t2", "t1"]);
    assert!(page.hits.iter().all(|h| h.snippet.is_none()));
}

#[test]
fn browse_mode_respects_filters() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: String::new(),
            sources: vec!["whatsapp".into()],
            author: "mom".into(),
            from_date: "2026-01-20".into(),
            to_date: "2026-01-20".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].message.external_id, "w1");
}

#[test]
fn combined_filters_still_count_consistently() {
    let s = seeded();
    let page = s
        .search_page(&SearchQuery {
            text: "the".into(),
            sources: vec!["whatsapp".into(), "telegram".into()],
            from_date: "2026-03-01".into(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.total, 2, "two telegram 'the' hits on Mar 1 (t1, t2)");
}

#[test]
fn chat_context_returns_ordered_neighbors() {
    let s = seeded();
    let middle = s
        .search_page(&SearchQuery {
            text: "shipping".into(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap()
        .hits[0]
        .message
        .clone();

    let ctx: ChatContext = s.chat_context(middle.id, 5).unwrap().expect("exists");
    assert_eq!(ctx.message.id, middle.id);
    assert_eq!(ctx.before.len(), 0, "t1 is the first message in devs");
    let ids: Vec<&str> = ctx.after.iter().map(|m| m.external_id.as_str()).collect();
    assert_eq!(ids, vec!["t2", "t3"]);
}

#[test]
fn chat_context_radius_is_respected_and_current_not_repeated() {
    let s = seeded();
    let last = s
        .search_page(&SearchQuery {
            text: "milliseconds".into(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap()
        .hits[0]
        .message
        .clone();
    let ctx = s.chat_context(last.id, 1).unwrap().unwrap();
    let ids: Vec<&str> = ctx.before.iter().map(|m| m.external_id.as_str()).collect();
    assert_eq!(ids, vec!["t2"]);
    assert!(ctx.after.is_empty());
    assert!(ctx
        .before
        .iter()
        .chain(std::iter::once(&ctx.message))
        .chain(ctx.after.iter())
        .all(|m| m.chat_id == "devs" && m.source == "telegram"));
}

#[test]
fn chat_context_without_chat_id_has_no_peers() {
    let s = Storage::open_in_memory().unwrap();
    let mut lonely = NewMessage::new("mbox", "l1", "a", "2026-01-01T00:00:00Z", "solo message");
    lonely.chat_id = String::new();
    let id = match s.insert_message(&lonely).unwrap() {
        vault_core::message::InsertOutcome::Inserted(id) => id,
        _ => unreachable!(),
    };
    let ctx = s.chat_context(id, 3).unwrap().expect("exists");
    assert!(ctx.before.is_empty() && ctx.after.is_empty());
}

#[test]
fn chat_context_breaks_ties_by_id_and_skips_other_chats() {
    let s = Storage::open_in_memory().unwrap();
    // Same timestamp in one chat, plus a same-timestamp message in another
    // chat that must not leak in.
    for (external_id, chat) in [("a1", "c1"), ("a2", "c1"), ("a3", "c1")] {
        s.insert_message(&chat_msg(
            "mbox",
            external_id,
            chat,
            "x",
            "2026-01-01T00:00:00Z",
            &format!("body {external_id}"),
        ))
        .unwrap();
    }
    s.insert_message(&chat_msg(
        "mbox",
        "b1",
        "c2",
        "y",
        "2026-01-01T00:00:00Z",
        "body b1",
    ))
    .unwrap();
    s.insert_message(&chat_msg(
        "mbox",
        "a4",
        "c1",
        "x",
        "2026-01-02T00:00:00Z",
        "body a4",
    ))
    .unwrap();

    let target = s
        .search_page(&SearchQuery {
            text: "a2".into(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap()
        .hits[0]
        .message
        .clone();
    let ctx = s.chat_context(target.id, 10).unwrap().unwrap();
    let ids: Vec<&str> = ctx
        .before
        .iter()
        .chain(std::iter::once(&ctx.message))
        .chain(ctx.after.iter())
        .map(|m| m.external_id.as_str())
        .collect();
    assert_eq!(ids, vec!["a1", "a2", "a3", "a4"]);
}

#[test]
fn chat_context_missing_message_is_none() {
    let s = seeded();
    assert!(s.chat_context(999_999, 3).unwrap().is_none());
}

// ----------------------------------------------------------------------
// Keyset cursors (deep pages)
// ----------------------------------------------------------------------

/// Cursor-walk and offset-walk helpers: the cursor pages must return
/// exactly the rows and totals the equivalent offset pages return.
fn offset_walk_v1(s: &Storage, base: &SearchQuery, limit: u32) -> (Vec<Vec<i64>>, u64) {
    let mut pages = Vec::new();
    let mut offset = 0;
    let mut total;
    loop {
        let page = s
            .search_page(&SearchQuery {
                limit,
                offset,
                ..base.clone()
            })
            .unwrap();
        total = page.total;
        pages.push(page.hits.iter().map(|h| h.message.id).collect::<Vec<i64>>());
        if page.hits.is_empty() || offset as u64 + page.hits.len() as u64 >= total {
            break;
        }
        offset += limit;
    }
    (pages, total)
}

fn cursor_walk_v1(s: &Storage, base: &SearchQuery, limit: u32) -> Vec<Vec<i64>> {
    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let page = s
            .search_page(&SearchQuery {
                limit,
                cursor,
                ..base.clone()
            })
            .unwrap();
        pages.push(page.hits.iter().map(|h| h.message.id).collect::<Vec<i64>>());
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    pages
}

#[test]
fn cursor_paging_matches_offset_pages_browse_and_relevance() {
    let s = Storage::open_in_memory().unwrap();
    // Dated rows with a shared instant (the id tiebreak), unknown-time
    // rows (the NULL boundary), and a non-matching filler row.
    let rows = [
        ("u1", "2026-01-01T09:00:00Z", "vault needle one"),
        ("u2", "2026-01-02T09:00:00Z", "vault needle two"),
        ("u3", "2026-01-02T09:00:00Z", "vault needle three ties"),
        ("u4", "2026-01-03T09:00:00Z", "vault needle four"),
        ("u5", "", "vault needle five no time"),
        ("u6", "", "vault needle six no time"),
        ("u7", "2026-01-04T09:00:00Z", "unrelated filler text"),
    ];
    for (id, sent, body) in rows {
        s.insert_message(&chat_msg("mbox", id, "c", "a", sent, body))
            .unwrap();
    }

    for (label, base) in [
        ("browse", SearchQuery::default()),
        (
            "relevance",
            SearchQuery {
                text: "needle".into(),
                ..SearchQuery::default()
            },
        ),
    ] {
        let (offset_pages, total) = offset_walk_v1(&s, &base, 2);
        let cursor_pages = cursor_walk_v1(&s, &base, 2);
        assert_eq!(cursor_pages, offset_pages, "{label}: identical pages");
        assert_eq!(total, if label == "browse" { 7 } else { 6 });
        // The boundary page keeps the unknown-time rows in order: the
        // browse walk crosses from dated [1] into unknowns [6].
        if label == "browse" {
            assert_eq!(
                cursor_pages,
                vec![vec![7, 4], vec![3, 2], vec![1, 6], vec![5]],
                "no row skipped or repeated across the NULL boundary"
            );
        }
    }

    // Cursor pages report offset 0 and the same exact totals.
    let page = s
        .search_page(&SearchQuery {
            text: "needle".into(),
            limit: 2,
            cursor: Some(
                s.search_page(&SearchQuery {
                    text: "needle".into(),
                    limit: 2,
                    ..SearchQuery::default()
                })
                .unwrap()
                .next_cursor
                .unwrap(),
            ),
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.offset, 0);
    assert_eq!(page.total, 6);
}

#[test]
fn cursor_wins_over_offset_in_v1_pages() {
    let s = seeded();
    let p1 = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 2,
            ..SearchQuery::default()
        })
        .unwrap();
    let next = p1.next_cursor.clone().unwrap();
    // offset 1000 alone pages past the end; with a cursor it is ignored.
    let page = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 2,
            offset: 1000,
            cursor: Some(next),
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.hits.len(), 1, "the last of the three vault hits");
    assert_eq!(page.total, p1.total);
    // The page continues the cursor, not the offset.
    let all: Vec<i64> = p1
        .hits
        .iter()
        .chain(page.hits.iter())
        .map(|h| h.message.id)
        .collect();
    let mut unique = all.clone();
    unique.dedup();
    assert_eq!(all.len(), unique.len(), "no repeats across pages");
}

#[test]
fn cursor_past_the_filtered_end_is_an_empty_page_without_next_cursor() {
    let s = seeded();
    // A cursor at w2 (Jan 21, position 5 of the newest-first browse:
    // t3, t2, t1, e3, w2, w1, e2, e1).
    let p1 = s
        .search_page(&SearchQuery {
            text: String::new(),
            limit: 5,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(p1.hits.len(), 5);
    let cursor = p1.next_cursor.unwrap();
    // Continue under a telegram-only filter: every telegram row sorts
    // before the cursor, so the continuation is empty — while the
    // total still counts the whole filtered set.
    let page = s
        .search_page(&SearchQuery {
            text: String::new(),
            sources: vec!["telegram".into()],
            limit: 5,
            cursor: Some(cursor),
            ..SearchQuery::default()
        })
        .unwrap();
    assert!(page.hits.is_empty());
    assert_eq!(page.total, 3, "three telegram rows in the whole set");
    assert!(page.next_cursor.is_none());
}

#[test]
fn v1_cursor_rejects_wrong_mode_and_malformed_cursors() {
    let s = seeded();
    // A relevance cursor under a browse query.
    let relevance = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 2,
            ..SearchQuery::default()
        })
        .unwrap()
        .next_cursor
        .unwrap();
    let err = s
        .search_page(&SearchQuery {
            text: String::new(),
            cursor: Some(relevance),
            ..SearchQuery::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("cursor was issued"), "{err}");

    // A browse cursor under a relevance query.
    let browse = s
        .search_page(&SearchQuery {
            text: String::new(),
            limit: 2,
            ..SearchQuery::default()
        })
        .unwrap()
        .next_cursor
        .unwrap();
    let err = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            cursor: Some(browse),
            ..SearchQuery::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("cursor was issued"), "{err}");

    // Malformed wire text.
    for raw in ["not-a-cursor", ""] {
        let err = s
            .search_page(&SearchQuery {
                text: "vault".into(),
                cursor: Some(raw.into()),
                ..SearchQuery::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("malformed cursor"), "{err}");
    }
}
