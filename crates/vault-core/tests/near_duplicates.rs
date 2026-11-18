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
//!
//! Query-time folding (P2.14, near-dup 3/5): one pass over the ranked
//! page folds hits within Hamming distance 3 of an already-kept
//! higher-ranked representative into it —
//!
//! - a forwarded-copy fixture (original + forward + quoted reply)
//!   collapses to one representative carrying the member count and
//!   ids, while an unrelated message stays a separate hit;
//! - representatives keep their exact unfolded order;
//! - cursor pages continue at the pre-fold tail, so a folded row is
//!   never re-read;
//! - hits without a fingerprint neither fold nor absorb others.

use rusqlite::Connection;
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::search2::SearchArchiveRequest;
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

// ---------------------------------------------------------------------------
// P2.14: query-time fold of near-duplicate hits (near-dup 3/5)
// ---------------------------------------------------------------------------

/// The forwarded copy: the same body under the one-line header a mail
/// client prepends when forwarding.
fn forwarded_copy() -> String {
    format!("Begin forwarded message:\n\n{ANNOUNCEMENT}")
}

/// The quoted reply with the short top line a client sends — the fold
/// fixture's third member. Pairwise distances to the original and the
/// forward stay within the threshold (the longer "Done, thanks." top
/// text of the foundation's helper adds a bit over).
fn short_quoted_reply() -> String {
    let quoted: String = ANNOUNCEMENT
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("Thanks.\n\nOn Mon, Ana wrote:\n{quoted}")
}

/// A body that matches the same text search but shares no message
/// content with the announcement — the second, distinct hit the
/// results list owes the reader.
const UNRELATED: &str = "renovation budget for the community hall approved by \
the council after long debates about wheelchair ramps roof insulation quotes \
and the reserve fund ceiling for the next fiscal year";

fn msg_at(source: &str, external_id: &str, body: &str, sent_at: &str) -> NewMessage {
    NewMessage::new(source, external_id, "ana", sent_at, body)
}

fn search(s: &Storage, request: &SearchArchiveRequest) -> vault_core::search2::SearchArchivePage {
    s.search_archive(request).unwrap()
}

#[test]
fn search_folds_forwarded_copies_into_one_representative() {
    // Fixture sanity: whichever of the trio ranks first must fold the
    // other two, so all three pairwise distances stay within the
    // threshold, and the unrelated body stays outside every cluster.
    let trio = [
        simhash64(ANNOUNCEMENT),
        simhash64(&forwarded_copy()),
        simhash64(&short_quoted_reply()),
    ];
    let pairwise = [
        ("forward-reply", hamming_distance(trio[1], trio[2])),
        ("original-forward", hamming_distance(trio[0], trio[1])),
        ("original-reply", hamming_distance(trio[0], trio[2])),
    ];
    for (name, distance) in pairwise {
        assert!(
            distance <= 3,
            "fixture trio must be pairwise near-duplicates ({name} = {distance})"
        );
    }
    for &a in trio.iter() {
        let distance = hamming_distance(a, simhash64(UNRELATED));
        assert!(
            distance > 3,
            "unrelated fixture must stay out of the cluster, got {distance}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let s = Storage::open(&db).unwrap();
    let original = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-1",
            ANNOUNCEMENT,
            "2026-11-04T09:00:00Z",
        ),
    );
    let forward = inserted_id(
        &s,
        &msg_at(
            "email-b",
            "announce-2",
            &forwarded_copy(),
            "2026-11-03T09:00:00Z",
        ),
    );
    let reply = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-3",
            &short_quoted_reply(),
            "2026-11-02T09:00:00Z",
        ),
    );
    let unrelated = inserted_id(
        &s,
        &msg_at("email-a", "budget-1", UNRELATED, "2026-11-01T09:00:00Z"),
    );
    drop(s);

    let request = SearchArchiveRequest {
        text: "renovation".into(),
        ..Default::default()
    };

    // With import-time fingerprints present, the three near-identical
    // copies collapse into their highest-ranked member.
    let s = Storage::open(&db).unwrap();
    let folded = search(&s, &request);
    assert_eq!(folded.total, 4, "total keeps its unfolded snapshot meaning");
    assert_eq!(
        folded.hits.len(),
        2,
        "three copies fold; the unrelated hit stays"
    );

    let cluster = folded
        .hits
        .iter()
        .find(|hit| hit.cluster_count > 1)
        .expect("one folded representative");
    assert_eq!(cluster.cluster_count, 3);
    let mut folded_members = cluster.cluster_member_ids.clone();
    folded_members.sort_unstable();
    let mut expected_members = [original, forward, reply]
        .into_iter()
        .filter(|id| *id != cluster.id)
        .collect::<Vec<_>>();
    expected_members.sort_unstable();
    assert_eq!(folded_members, expected_members);

    let separate = folded
        .hits
        .iter()
        .find(|hit| hit.id == unrelated)
        .expect("the unrelated message stays its own hit");
    assert_eq!(separate.cluster_count, 1);
    assert!(separate.cluster_member_ids.is_empty());
    drop(s);

    // Control: the identical archive with the fingerprint rows
    // removed (the pre-migration archive shape) never folds. Its hit
    // order is the unfolded reference the representatives must keep.
    let conn = Connection::open(&db).unwrap();
    conn.execute("DELETE FROM message_fingerprints", [])
        .unwrap();
    drop(conn);
    let s = Storage::open(&db).unwrap();
    let unfolded = search(&s, &request);
    assert_eq!(unfolded.hits.len(), 4);
    assert!(unfolded
        .hits
        .iter()
        .all(|hit| { hit.cluster_count == 1 && hit.cluster_member_ids.is_empty() }));

    // Representative order unchanged: the folded page's ids appear in
    // the unfolded order (subsequence), and every folded member sits
    // strictly after its representative in that order.
    let unfolded_ids: Vec<i64> = unfolded.hits.iter().map(|hit| hit.id).collect();
    let mut remainder = unfolded_ids.as_slice();
    for id in folded.hits.iter().map(|hit| hit.id) {
        let position = remainder
            .iter()
            .position(|candidate| *candidate == id)
            .expect("kept hits come from the unfolded order");
        remainder = &remainder[position + 1..];
    }
    let representative_at = unfolded_ids
        .iter()
        .position(|id| *id == cluster.id)
        .unwrap();
    for member in &cluster.cluster_member_ids {
        let member_at = unfolded_ids.iter().position(|id| id == member).unwrap();
        assert!(
            member_at > representative_at,
            "members fold into a higher-ranked representative"
        );
    }
}

#[test]
fn cursor_pages_continue_at_the_pre_fold_tail() {
    // Browse order is timestamp-controlled: original, forward, reply,
    // unrelated. A two-row page folds to one hit whose cluster covers
    // only this page; the cursor still names the forward (the pre-fold
    // tail), so page two starts at the reply — a folded row is never
    // re-read.
    let s = Storage::open_in_memory().unwrap();
    let original = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-1",
            ANNOUNCEMENT,
            "2026-11-04T09:00:00Z",
        ),
    );
    let forward = inserted_id(
        &s,
        &msg_at(
            "email-b",
            "announce-2",
            &forwarded_copy(),
            "2026-11-03T09:00:00Z",
        ),
    );
    let reply = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-3",
            &short_quoted_reply(),
            "2026-11-02T09:00:00Z",
        ),
    );
    let unrelated = inserted_id(
        &s,
        &msg_at("email-a", "budget-1", UNRELATED, "2026-11-01T09:00:00Z"),
    );

    let page_one = search(
        &s,
        &SearchArchiveRequest {
            text: String::new(),
            limit: Some(2),
            ..Default::default()
        },
    );
    assert_eq!(page_one.total, 4);
    assert_eq!(page_one.hits.len(), 1);
    assert_eq!(page_one.hits[0].id, original);
    assert_eq!(page_one.hits[0].cluster_count, 2);
    assert_eq!(page_one.hits[0].cluster_member_ids, vec![forward]);
    let cursor = page_one.next_cursor.clone().expect("two rows follow");

    let page_two = search(
        &s,
        &SearchArchiveRequest {
            text: String::new(),
            limit: Some(2),
            cursor: Some(cursor),
            ..Default::default()
        },
    );
    let page_two_ids: Vec<i64> = page_two.hits.iter().map(|hit| hit.id).collect();
    assert_eq!(page_two_ids, vec![reply, unrelated]);
    // Per-page clusters: the reply's cluster-mates were page one's;
    // this page sees only the unrelated hit beside it.
    assert!(page_two
        .hits
        .iter()
        .all(|hit| hit.cluster_count == 1 && hit.cluster_member_ids.is_empty()));
    assert!(page_two.next_cursor.is_none());
}

#[test]
fn hits_without_a_fingerprint_neither_fold_nor_absorb() {
    // The pre-migration archive shape: the top-ranked announcement
    // copy has no fingerprint row (no backfill yet). It passes through
    // untouched, absorbs nothing, and the second copy becomes the
    // representative for the quoted reply.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let s = Storage::open(&db).unwrap();
    let original = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-1",
            ANNOUNCEMENT,
            "2026-11-04T09:00:00Z",
        ),
    );
    let copy = inserted_id(
        &s,
        &msg_at(
            "email-b",
            "announce-2",
            ANNOUNCEMENT,
            "2026-11-03T09:00:00Z",
        ),
    );
    let reply = inserted_id(
        &s,
        &msg_at(
            "email-a",
            "announce-3",
            &short_quoted_reply(),
            "2026-11-02T09:00:00Z",
        ),
    );
    let unrelated = inserted_id(
        &s,
        &msg_at("email-a", "budget-1", UNRELATED, "2026-11-01T09:00:00Z"),
    );
    drop(s);

    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "DELETE FROM message_fingerprints WHERE message_id = ?1",
        [original],
    )
    .unwrap();
    drop(conn);

    let s = Storage::open(&db).unwrap();
    let page = search(
        &s,
        &SearchArchiveRequest {
            text: String::new(),
            ..Default::default()
        },
    );
    assert_eq!(page.total, 4);
    let page_ids: Vec<i64> = page.hits.iter().map(|hit| hit.id).collect();
    assert_eq!(page_ids, vec![original, copy, unrelated]);

    let untouched = &page.hits[0];
    assert_eq!(untouched.id, original);
    assert_eq!(untouched.cluster_count, 1);
    assert!(untouched.cluster_member_ids.is_empty());

    let representative = &page.hits[1];
    assert_eq!(representative.id, copy);
    assert_eq!(representative.cluster_count, 2);
    assert_eq!(representative.cluster_member_ids, vec![reply]);

    assert_eq!(page.hits[2].id, unrelated);
    assert_eq!(page.hits[2].cluster_count, 1);
}
