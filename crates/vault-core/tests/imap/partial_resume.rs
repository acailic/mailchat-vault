//! Scenario: partial sync resume — a sync interrupted mid-folder (the
//! server breaks, or the connection dies between batches) must continue
//! from the persisted batch cursor on the next round, fetching only what
//! it missed and never storing a duplicate.

use crate::imap_mock::MockMailbox;
use crate::{cursor, mails, summary_for, uid_set, Account, USER};
use vault_core::storage::Storage;

/// The connection dies right after the first batch of 110 mails is fully
/// delivered (the process-crash equivalent): the round reports the
/// partial counts, the cursor sits at the batch boundary, and the next
/// round fetches exactly the remaining batches.
#[test]
fn connection_loss_mid_folder_resumes_from_the_persisted_cursor() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(110, "drop")));
    // Drop the connection immediately after the 1st UID FETCH completes.
    account.server.drop_after_uid_fetch_once(Some(1));

    let storage = Storage::open_in_memory().unwrap();

    // Round 1: 50 mails stored, then the socket closes under the client.
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(
        s.error.is_some(),
        "the severed connection must surface as an error"
    );
    assert_eq!(s.seen, 50, "the delivered batch's counts survive the cut");
    assert_eq!(s.inserted, 50);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 50)));
    assert_eq!(storage.stats().unwrap().total, 50);

    // Round 2: resumes from UID 51 — batches 51..=100 and 101..=110, no
    // refetch of the first batch, no duplicates.
    let fetches_before = account.server.log().fetch_sets.len();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert_eq!(s.seen, 60);
    assert_eq!(s.inserted, 60);

    let fetches = &account.server.log().fetch_sets[fetches_before..];
    assert_eq!(
        fetches,
        &[uid_set(51..=100u32), uid_set(101..=110u32),],
        "the resume must fetch exactly the two remaining batches"
    );
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 110)));
    assert_eq!(
        storage.stats().unwrap().total,
        110,
        "no duplicates after the resume"
    );
}

/// The server answers `BAD` deep in the folder (third batch of a
/// 110-mail scan): two batches are stored with the cursor at 100, and the
/// retry fetches only UIDs 101..=110.
#[test]
fn server_error_deep_in_the_folder_resumes_without_refetch() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(110, "deep")));
    account.server.fail_uid_fetch_once(Some(3));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(s.error.is_some());
    assert_eq!(
        s.seen, 100,
        "two full batches were stored before the failure"
    );
    assert_eq!(s.inserted, 100);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 100)));

    let fetches_before = account.server.log().fetch_sets.len();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert_eq!((s.seen, s.inserted), (10, 10));

    let fetches = &account.server.log().fetch_sets[fetches_before..];
    assert_eq!(fetches, &[uid_set(101..=110u32)]);
    assert_eq!(storage.stats().unwrap().total, 110);
}

/// Interruption and reset combined: the sync dies after batch 1, and the
/// folder is then recreated with a new UIDVALIDITY before the retry. The
/// retry starts over as a reset scan, and `(source, external_id)` dedup
/// keeps the archive duplicate-free across the whole story.
#[test]
fn resume_after_an_interruption_and_a_reset_dedups_everything() {
    let account = Account::start(USER);
    let hundred = mails(100, "mix");
    account.set_mailbox("INBOX", MockMailbox::new(7, hundred.clone()));
    account.server.drop_after_uid_fetch_once(Some(1));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(s.error.is_some());
    assert_eq!(s.inserted, 50);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 50)));

    // Before the retry, the server recreates the folder under validity 9
    // with the same hundred mails (UIDs reassigned).
    account.set_mailbox("INBOX", MockMailbox::new(9, hundred));

    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert!(s.reset, "the validity change overrides the partial cursor");
    assert_eq!(s.seen, 100, "the reset rescans every UID from 1");
    assert_eq!(
        (s.skipped, s.inserted),
        (50, 50),
        "the 50 stored before the cut dedup; the 50 never fetched are stored"
    );
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((9, 100)));
    assert_eq!(storage.stats().unwrap().total, 100);
}
