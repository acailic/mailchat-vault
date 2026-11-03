//! Scenario: the M2.3 atomic batch commit and the durable failed-item
//! ledger. The write unit of a sync is one fetch batch — messages,
//! ledger updates, and the checkpoint advance land in one transaction —
//! and every accounted-but-unstorable item (an unparseable mail, a uid
//! the server searched but never delivered) is recorded durably with a
//! fixed reason, retried by later rounds, and cleared on resolution or
//! on a UIDVALIDITY reset.

use crate::imap_mock::MockMailbox;
use crate::{cursor, find, mails, summary_for, Account, USER};
use vault_core::imap::REASON_MISSING_FETCH;
use vault_core::imap::REASON_UNPARSEABLE;
use vault_core::storage::Storage;

/// A raw mail that fails to parse (a torn header continuation — the
/// same shape `imap_sync.rs` proves unparseable).
const BROKEN_MAIL: &[u8] = b"   torn header continuation\r\nSubject: never parses\r\n\r\nbody";

/// One ledger row as the visible accounting sees it.
fn ledger_row(storage: &Storage, user: &str) -> vault_core::storage::FailedItem {
    let instance = crate::instance_of(storage, user);
    let rows = storage.list_failed_items(Some(&instance)).unwrap();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one ledger row, got {rows:?}"
    );
    rows[0].clone()
}

/// A searched-but-never-delivered uid is ledgered as a missing fetch,
/// the cursor still advances past it (the ledger remembers it), and the
/// archive holds exactly what was delivered.
#[test]
fn missing_fetch_is_ledgered_and_the_cursor_advences() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "miss")));
    // UID 3 is searched, then silently omitted from the FETCH response.
    account.server.hide_uid_from_fetch_once(3);

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert_eq!(s.seen, 3, "the missing uid is accounted, not ignored");
    assert_eq!(s.inserted, 2);
    assert_eq!(s.failed, 1);
    // The cursor advances past the missing uid: it is ledgered, not lost.
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 3)));
    assert_eq!(storage.stats().unwrap().total, 2);

    let row = ledger_row(&storage, USER);
    assert_eq!(
        (row.folder.as_str(), row.uid, row.uidvalidity),
        ("INBOX", 3, 7)
    );
    assert_eq!(row.reason, REASON_MISSING_FETCH);
    assert_eq!(row.attempts, 1);
}

/// The ledgered missing fetch resolves on the next round: the retry pass
/// re-fetches it (still below the fetch range), stores it, and clears
/// the ledger row — the self-healing accounting.
#[test]
fn missing_fetch_resolves_on_the_next_round() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "heal")));
    account.server.hide_uid_from_fetch_once(3);

    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);
    assert_eq!(ledger_row(&storage, USER).uid, 3);

    // Round 2: nothing new above the cursor, but the retry pass fetches
    // uid 3 — the hide knob was one-shot, so it arrives now.
    let fetches_before = account.server.log().fetch_sets.len();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 1);
    assert_eq!(s.resolved_failures, 1);
    assert_eq!(s.failed, 0);
    assert_eq!(
        account.server.log().fetch_sets[fetches_before..],
        [crate::uid_set([3u32])],
        "the retry must fetch exactly the ledgered uid"
    );
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 3)));
    assert_eq!(storage.stats().unwrap().total, 3);
    // The ledger is empty again: the failure healed durably.
    let instance = crate::instance_of(&storage, USER);
    assert!(
        storage
            .list_failed_items(Some(&instance))
            .unwrap()
            .is_empty(),
        "a resolved failure must leave no ledger row"
    );
    assert_eq!(find(&storage, "healbody3").len(), 1);
}

/// An unparseable mail stays in the ledger with its reason, and every
/// later round retries it and bumps the attempt count — durable
/// accounting of what fell and why, with the retry history visible.
#[test]
fn unparseable_mail_is_ledgered_and_retried_each_round() {
    let account = Account::start(USER);
    account.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![BROKEN_MAIL.to_vec(), mails(1, "ok").remove(0)]),
    );

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.failed, 1);
    assert_eq!(s.inserted, 1);

    let row = ledger_row(&storage, USER);
    assert_eq!(row.uid, 1);
    assert_eq!(row.reason, REASON_UNPARSEABLE);
    assert_eq!(row.attempts, 1);

    // Round 2 and 3 retry the unparseable uid; it keeps failing, and the
    // attempt count keeps climbing — the ledger says what fell, why, and
    // how often the vault has tried.
    for expected_attempts in [2u32, 3] {
        let summaries = account.sync_round(&storage, &["INBOX"]);
        let s = summary_for(&summaries, "INBOX");
        assert_eq!(s.failed, 1, "the retry re-fails the unparseable mail");
        assert_eq!(s.resolved_failures, 0);
        assert_eq!(ledger_row(&storage, USER).attempts, expected_attempts);
    }
    // The parseable mail was never duplicated.
    assert_eq!(storage.stats().unwrap().total, 1);
}

/// A UIDVALIDITY reset adopts the new validity in the same atomic commit
/// that clears the folder's ledger rows from the old validity: the rescan
/// re-derives the truth for every uid, so stale failures from an
/// abandoned UID namespace never linger.
#[test]
fn uidvalidity_reset_clears_the_old_validity_ledger() {
    let account = Account::start(USER);
    // UID 2 is broken under validity 7.
    account.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mails(1, "keep").remove(0), BROKEN_MAIL.to_vec()]),
    );
    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);
    let row = ledger_row(&storage, USER);
    assert_eq!((row.uid, row.uidvalidity), (2, 7));

    // The folder is recreated under validity 9 with both mails intact —
    // the recreated uid 2 is a *good* copy this time.
    account.set_mailbox(
        "INBOX",
        MockMailbox::new(
            9,
            vec![mails(1, "keep").remove(0), mails(1, "fixed").remove(0)],
        ),
    );
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(s.reset);
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 1, "the fixed copy of uid 2 stores");
    assert_eq!(s.skipped, 1, "the known mail dedups");

    // Old-validity row gone, nothing re-ledgered: the ledger is empty.
    let instance = crate::instance_of(&storage, USER);
    assert!(
        storage
            .list_failed_items(Some(&instance))
            .unwrap()
            .is_empty(),
        "a reset must clear the abandoned validity's failures"
    );
    assert_eq!(storage.stats().unwrap().total, 2);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((9, 2)));
}

/// The ledger is durable accounting: it survives closing and reopening
/// the vault file, exactly like the cursors it complements.
#[test]
fn the_ledger_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");

    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "durable")));
    account.server.hide_uid_from_fetch_once(2);

    {
        let storage = Storage::open(&path).unwrap();
        account.sync_round(&storage, &["INBOX"]);
        assert_eq!(ledger_row(&storage, USER).uid, 2);
    }

    // Reopen: the failure is still visible, still retryable.
    let storage = Storage::open(&path).unwrap();
    let row = ledger_row(&storage, USER);
    assert_eq!((row.uid, row.reason.as_str()), (2, REASON_MISSING_FETCH));
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.resolved_failures, 1);
    assert_eq!(storage.stats().unwrap().total, 2);
    let instance = crate::instance_of(&storage, USER);
    assert!(storage
        .list_failed_items(Some(&instance))
        .unwrap()
        .is_empty());
}
