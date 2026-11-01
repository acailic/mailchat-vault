//! DST regression: a day bound must resolve local midnight for THAT date
//! (per-date historical offset), not freeze the process's current offset.
//! Own test binary (own process) so the pinned TZ cannot leak into or race
//! the other integration tests.
//!
//! Season-independent: the summer pair (2026-07-15 under Zagreb DST, +02:00)
//! fails any frozen-offset implementation that runs in winter, and the winter
//! pair (2026-01-15 at +01:00) fails one that runs in summer — so a frozen
//! `Local::now().fixed_offset()` fails at least one assertion year-round,
//! while per-date resolution passes all of them.

use vault_core::message::NewMessage;
use vault_core::query::SearchQuery;
use vault_core::storage::Storage;

#[test]
fn historical_dst_day_filters_by_its_own_offset() {
    std::env::set_var("TZ", "Europe/Zagreb");
    let _ = chrono::Local::now(); // resolve with the pinned TZ

    let s = Storage::open_in_memory().unwrap();
    for (external_id, sent_at, body) in [
        (
            "dst-summer",
            "2026-07-14T22:30:00Z", // displays 2026-07-15 00:30 local (UTC+2)
            "vault summer heat",
        ),
        (
            "dst-winter",
            "2026-01-14T22:30:00Z", // displays 2026-01-14 23:30 local (UTC+1)
            "vault winter snow",
        ),
    ] {
        let mut m = NewMessage::new("mbox", external_id, "ana", sent_at, body);
        m.chat_id = "c".into();
        s.insert_message(&m).unwrap();
    }

    let total = |term: &str, from: &str, to: &str| -> u64 {
        s.search_page(&SearchQuery {
            text: term.into(),
            from_date: from.into(),
            to_date: to.into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap()
        .total
    };

    // Summer: midnight of 2026-07-15 is 2026-07-14T22:00:00Z (DST +02:00).
    assert_eq!(
        total("summer", "2026-07-15", ""),
        1,
        "from = display day includes"
    );
    assert_eq!(
        total("summer", "", "2026-07-15"),
        1,
        "to = display day includes"
    );
    assert_eq!(
        total("summer", "", "2026-07-14"),
        0,
        "to before display day excludes"
    );
    assert_eq!(
        total("summer", "2026-07-16", ""),
        0,
        "from after display day excludes"
    );

    // Winter: midnight of 2026-01-15 is 2026-01-14T23:00:00Z (+01:00), so the
    // same wall-clock message lands inside Jan 14 and must not match Jan 15.
    assert_eq!(
        total("winter", "2026-01-15", ""),
        0,
        "wrong-season from must exclude"
    );
    assert_eq!(
        total("winter", "", "2026-01-14"),
        1,
        "to = display day includes"
    );
    assert_eq!(
        total("winter", "", "2026-01-13"),
        0,
        "to before display day excludes"
    );
    assert_eq!(
        total("winter", "2026-01-16", ""),
        0,
        "from after display day excludes"
    );
}
