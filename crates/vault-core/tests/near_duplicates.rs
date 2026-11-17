//! Near-duplicate fingerprints (P2.12, storage slice): every message
//! inserted through the public path persists a 64-bit SimHash
//! fingerprint over its body, computed once at import so query-time
//! folding (follow-up slice) never recomputes it. The contracts proven
//! here mirror the slice's acceptance behavior —
//!
//! - identical text yields the same fingerprint;
//! - forwarded copies and quoted-reply variants sit within Hamming
//!   distance 3;
//! - unrelated text is far away;
//! - Cyrillic text hashes stably;
//! - inserting a message through the public insert path persists the
//!   expected fingerprint row, in the same transaction.

use rusqlite::Connection;
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::simhash::{hamming_distance, simhash64};
use vault_core::storage::Storage;

/// The body of the announcement whose copies real archives carry —
/// forwarded chains, mailing-list echoes, quoted replies. Long enough
/// that a quote-marker or header pass perturbs only a small fraction
/// of its shingles.
const ANNOUNCEMENT: &str = "\
The renovation crew confirmed the schedule for next week in a message that \
also listed the materials arriving on site each day. Monday morning they \
will remove the old kitchen tiles and patch the wall behind the stove \
before the electrician arrives with his apprentice. Tuesday the electrician \
installs the new outlets under the cabinets, replaces the junction box in \
the hallway, and moves the light switch by the door closer to the frame. \
Wednesday the painter starts with the ceiling in the living room and \
hallway while the plaster dries in the bedroom and the putty sets around \
the window frames left from the October storm. Thursday the floor crew \
lays the oak boards in the study, sands the joints level, and seals them \
before the furniture returns from the storage unit on Fourth Street. \
Friday the plumber connects the dishwasher, hangs the new radiator in the \
bathroom, and tests every faucet and pipe joint for leaks under full \
pressure while the meter runs. Saturday we walk through the apartment \
together with the contractor and note anything that needs a second pass \
before the final invoice is issued and the retainers come off. Sunday the \
rooms rest, the ventilation runs, and we carry the boxes back up from the \
cellar while the paint cures and the floors settle. The building manager \
asked us to keep the staircase clear of dust sheets during all of this, \
and the neighbors on the third floor expect quiet hours after eight in \
the evening as always.\n\
Following up on that schedule, the supplier confirmed delivery slots for \
every material on the list. The tiles arrive Tuesday noon on a pallet \
that must be moved inside the same afternoon because the courtyard locks \
overnight and the forecast promises frost. The oak boards need two days \
to acclimatize in the study before the floor crew can open the packages, \
so they come Thursday rather than Wednesday as first planned. The \
radiator and the faucet fittings are already in the cellar, checked \
against the invoice, with the receipts clipped to the folder we keep for \
the building manager. The painter asked for the color samples back by \
Friday morning so he can order enough ceiling white for both coats over \
the weekend. We should also decide about the hallway mirror before the \
glazier finishes the shelf it stands on; the measurements are in the same \
folder, second page, next to the window putty receipt. If any of the \
deliveries slip, the whole week shifts by a day and the second pass \
moves to the following Monday, which still fits before the holidays.";

/// One short reply line plus an attribution header plus the full
/// original under `>` quote markers — the shape mail clients produce
/// for answered and forwarded mail. Quote markers are word separators,
/// so the quoted body contributes no new shingles (Broder's set
/// semantics); only the reply line, the header, and their boundary
/// windows differ.
fn quoted_reply() -> String {
    let quoted: String = ANNOUNCEMENT
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("Done, thanks.\n\nOn Mon, Ana wrote:\n{quoted}")
}

fn msg(source: &str, external_id: &str, body: &str) -> NewMessage {
    NewMessage::new(source, external_id, "ana", "2026-11-01T09:00:00Z", body)
}

fn inserted_id(s: &Storage, msg: &NewMessage) -> i64 {
    match s.insert_message(msg).unwrap() {
        InsertOutcome::Inserted(id) | InsertOutcome::Duplicate(id) => id,
    }
}

#[test]
fn identical_text_produces_identical_fingerprints() {
    assert_eq!(simhash64(ANNOUNCEMENT), simhash64(ANNOUNCEMENT));
    // A re-send under a different timestamp still carries the same
    // body text, so the fingerprint — a function of the body only —
    // does not move.
    assert_eq!(
        simhash64(ANNOUNCEMENT),
        simhash64(&format!("{ANNOUNCEMENT}\n"))
    );
}

#[test]
fn forwarded_and_quoted_variants_land_within_near_duplicate_distance() {
    // The quoted reply: new top text above an attribution header and a
    // fully quoted original.
    let reply_distance = hamming_distance(simhash64(ANNOUNCEMENT), simhash64(&quoted_reply()));
    assert!(
        reply_distance <= 3,
        "quoted reply should sit within Hamming distance 3, got {reply_distance}"
    );

    // The forwarded copy: same text under a mail-client header and
    // footer.
    let forward = format!("Begin forwarded message:\n\n{ANNOUNCEMENT}\n\n—\nsent from my phone");
    let forward_distance = hamming_distance(simhash64(ANNOUNCEMENT), simhash64(&forward));
    assert!(
        forward_distance <= 3,
        "forwarded copy should sit within Hamming distance 3, got {forward_distance}"
    );

    // The mailing-list echo: the same body with a one-line list footer.
    let echo =
        format!("{ANNOUNCEMENT}\n\n________________________________\nRenovation mailing list");
    let echo_distance = hamming_distance(simhash64(ANNOUNCEMENT), simhash64(&echo));
    assert!(
        echo_distance <= 3,
        "list echo should sit within Hamming distance 3, got {echo_distance}"
    );
}

#[test]
fn unrelated_text_stays_far_away() {
    let other = "quarterly tax filing deadline extended by the revenue \
office until the end of march for electronic submissions";
    let distance = hamming_distance(simhash64(ANNOUNCEMENT), simhash64(other));
    assert!(
        distance > 10,
        "unrelated text should be far away, got {distance}"
    );
}

#[test]
fn cyrillic_text_fingerprints_are_stable() {
    let body = "Ремонтная бригада подтвердила расписание на следующую \
неделю. В понедельник утром они снимут старую плитку на кухне и \
заштукатурят стену за плитой. Во вторник электрик установит новые \
розетки под шкафами.";
    let first = simhash64(body);
    assert_eq!(first, simhash64(body));
    // Re-wrapped by a different mail client: same Cyrillic words,
    // different line breaks — identical shingles, identical
    // fingerprint.
    let rewrapped = "Ремонтная бригада подтвердила расписание\n\
на следующую неделю. В понедельник утром\n\
они снимут старую плитку на кухне и заштукатурят\n\
стену за плитой. Во вторник электрик\n\
установит новые розетки под шкафами.";
    assert_eq!(first, simhash64(rewrapped));
    assert_ne!(
        first,
        simhash64("Налоговая декларация продлена до конца марта для электронных подач.")
    );
}

#[test]
fn inserting_a_message_persists_its_fingerprint_row() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let s = Storage::open(&db).unwrap();

    let id = inserted_id(&s, &msg("email-a", "announce-1", ANNOUNCEMENT));
    let second = inserted_id(&s, &msg("email-a", "announce-2", "Done, thanks."));
    assert_ne!(id, second);
    drop(s);

    let conn = Connection::open(&db).unwrap();
    let stored: i64 = conn
        .query_row(
            "SELECT simhash64 FROM message_fingerprints WHERE message_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, simhash64(ANNOUNCEMENT) as i64);

    let stored_second: i64 = conn
        .query_row(
            "SELECT simhash64 FROM message_fingerprints WHERE message_id = ?1",
            [second],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_second, simhash64("Done, thanks.") as i64);

    // One row per message, exactly the rows just written.
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_fingerprints", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 2);

    // The fingerprint follows its message: deleting the archived row
    // cascades (the digest-keyed side-table pattern).
    conn.execute("DELETE FROM archive_messages WHERE id = ?1", [id])
        .unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_fingerprints", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn a_duplicate_insert_keeps_the_first_fingerprint_row() {
    let s = Storage::open_in_memory().unwrap();
    let id = inserted_id(&s, &msg("email-a", "announce-1", ANNOUNCEMENT));
    // Same source identity again: the archive keeps one row and one
    // fingerprint (idempotent dedup, no unique-constraint failure).
    match s
        .insert_message(&msg("email-a", "announce-1", ANNOUNCEMENT))
        .unwrap()
    {
        InsertOutcome::Duplicate(returned) => assert_eq!(returned, id),
        other => panic!("expected a duplicate outcome, got {other:?}"),
    }
}
