//! Scenario: per-folder error collection — a failing folder must never
//! abort the rest of the account sync. Every folder gets its own summary,
//! errors land in that folder's summary only, and the folders around the
//! failure sync completely.

use crate::{cursor, mails, summary_for, Account, USER};
use vault_core::storage::Storage;

/// A four-folder account where the third folder does not exist on the
/// server: the sync still returns one summary per folder, only the missing
/// one carries an error, and the other three archive fully.
#[test]
fn one_failing_folder_reports_its_error_and_the_rest_still_sync() {
    let account = Account::start(USER);
    account.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(7, mails(2, "in")),
    );
    account.set_mailbox(
        "Archive",
        crate::imap_mock::MockMailbox::new(3, mails(1, "ar")),
    );
    account.set_mailbox(
        "Sent",
        crate::imap_mock::MockMailbox::new(11, mails(1, "se")),
    );
    // "Missing" is deliberately never created on the server.

    let storage = Storage::open_in_memory().unwrap();
    let folders = ["INBOX", "Archive", "Missing", "Sent"];
    let summaries = account.sync_round(&storage, &folders);

    assert_eq!(summaries.len(), 4, "one summary per configured folder");

    let missing = summary_for(&summaries, "Missing");
    let err = missing
        .error
        .as_deref()
        .expect("the missing folder must carry its error");
    assert!(
        err.to_lowercase().contains("no such mailbox"),
        "unexpected error: {err}"
    );
    assert_eq!(missing.seen, 0);

    for ok in ["INBOX", "Archive", "Sent"] {
        let s = summary_for(&summaries, ok);
        assert_eq!(s.error, None, "{ok} must not fail because Missing did");
        assert_eq!(s.inserted as usize, expected_count(ok), "{ok} inserted");
        assert!(
            cursor(&storage, USER, ok).is_some(),
            "{ok} must have a persisted cursor"
        );
    }

    // The failed folder left no cursor behind, and the archive holds
    // exactly the three healthy folders' mail.
    assert_eq!(cursor(&storage, USER, "Missing"), None);
    assert_eq!(storage.stats().unwrap().total, 4);

    fn expected_count(folder: &str) -> usize {
        match folder {
            "INBOX" => 2,
            _ => 1,
        }
    }
}

/// A folder that breaks *mid-scan* (one batch stored, the next batch's
/// FETCH answered `BAD`) keeps the counts it already accumulated, and the
/// folder configured after it still syncs to completion.
#[test]
fn a_mid_folder_breakage_keeps_partial_counts_and_later_folders() {
    let account = Account::start(USER);
    // 110 mails → three fetch batches (50 + 50 + 10).
    account.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(7, mails(110, "in")),
    );
    account.set_mailbox(
        "Archive",
        crate::imap_mock::MockMailbox::new(3, mails(1, "ar")),
    );
    // The 2nd UID FETCH on this server (INBOX's second batch) fails.
    account.server.fail_uid_fetch_once(Some(2));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX", "Archive"]);

    let inbox = summary_for(&summaries, "INBOX");
    assert!(
        inbox.error.is_some(),
        "the broken folder must carry its error"
    );
    assert_eq!(inbox.seen, 50, "counts from the stored first batch survive");
    assert_eq!(inbox.inserted, 50);
    assert_eq!(inbox.failed, 0);

    let archive = summary_for(&summaries, "Archive");
    assert_eq!(archive.error, None, "the later folder must still sync");
    assert_eq!(archive.inserted, 1);
    assert_eq!(storage.stats().unwrap().total, 51);
}

/// Worst case: every configured folder fails. The sync still returns one
/// summary per folder, each with its own error, and stores nothing —
/// instead of panicking or aborting on the first failure.
#[test]
fn every_folder_failing_still_returns_one_summary_per_folder() {
    let account = Account::start(USER);
    // No mailbox is ever created.
    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX", "Work", "Archive"]);

    assert_eq!(summaries.len(), 3);
    for s in &summaries {
        assert!(s.error.is_some(), "{} must report its own error", s.folder);
        assert_eq!(s.seen, 0);
    }
    assert_eq!(storage.stats().unwrap().total, 0);
}

/// A folder that failed one round (it did not exist yet) is picked up
/// normally once it appears: errors are transient per-folder state, not a
/// sticky poison.
#[test]
fn a_folder_that_failed_one_round_syncs_when_it_appears() {
    let account = Account::start(USER);
    account.set_mailbox(
        "INBOX",
        crate::imap_mock::MockMailbox::new(7, mails(1, "in")),
    );

    let storage = Storage::open_in_memory().unwrap();

    // Round 1: the Archive folder is not on the server yet.
    let summaries = account.sync_round(&storage, &["INBOX", "Archive"]);
    assert!(summary_for(&summaries, "Archive").error.is_some());
    assert_eq!(summary_for(&summaries, "INBOX").error, None);

    // Round 2: the folder exists now.
    account.set_mailbox(
        "Archive",
        crate::imap_mock::MockMailbox::new(3, mails(1, "ar")),
    );
    let summaries = account.sync_round(&storage, &["INBOX", "Archive"]);
    let archive = summary_for(&summaries, "Archive");
    assert_eq!(archive.error, None, "the earlier failure must not stick");
    assert!(archive.reset, "a first scan of a folder is a full scan");
    assert_eq!(archive.inserted, 1);

    // INBOX was untouched by either of Archive's states.
    let inbox = summary_for(&summaries, "INBOX");
    assert_eq!(inbox.error, None);
    assert_eq!(inbox.seen, 0, "INBOX cursor from round 1 still applies");
    assert_eq!(storage.stats().unwrap().total, 2);
}
