//! DST regression: a day bound must resolve local midnight for THAT date
//! (per-date historical offset), not freeze the process's current offset.
//! Own test binary (own process) so the pinned TZ cannot leak into or race
//! the other integration tests.
//!
//! Fails against the frozen-offset implementation from any timezone: there,
//! `from_date=2026-07-15` resolves to 2026-07-14T23:00:00Z (winter +01:00)
//! and the 22:30Z message is excluded; with per-date resolution Zagreb's
//! DST midnight is 2026-07-14T22:00:00Z (summer +02:00) and it is included.

use vault_core::message::NewMessage;
use vault_core::query::SearchQuery;
use vault_core::storage::Storage;

#[test]
fn historical_dst_day_filters_by_its_own_offset() {
    std::env::set_var("TZ", "Europe/Zagreb");
    let _ = chrono::Local::now(); // resolve with the pinned TZ

    let s = Storage::open_in_memory().unwrap();
    s.insert_message(&NewMessage::new(
        "mbox",
        "dst-1",
        "ana",
        "2026-07-14T22:30:00Z", // displays 2026-07-15 00:30 local (UTC+2)
        "vault during summer",
    ))
    .unwrap();

    let total = |from: &str, to: &str| -> u64 {
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

    assert_eq!(total("2026-07-15", ""), 1, "from = display day must include");
    assert_eq!(total("", "2026-07-15"), 1, "to = display day must include");
    assert_eq!(
        total("", "2026-07-14"),
        0,
        "to = day before the display day must exclude"
    );
}
