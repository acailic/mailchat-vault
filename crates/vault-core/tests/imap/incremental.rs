//! Scenario: incremental sync — after the first scan, each round fetches
//! only mail with UIDs above the persisted cursor. The server-side
//! evidence (UID SEARCH ranges and UID FETCH sets) proves old mail is
//! never re-downloaded.

use crate::imap_mock::MockMailbox;
use crate::{cursor, mail, mails, summary_for, uid_set, Account, USER};
use vault_core::storage::Storage;

/// Three growth rounds: initial scan, one new mail, three new mails. Each
/// round searches from `cursor+1` and fetches exactly the new UIDs.
#[test]
fn three_rounds_of_new_mail_fetch_only_the_new_uids() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "grow")));

    let storage = Storage::open_in_memory().unwrap();

    // Round 1: full scan of UIDs 1..=2.
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(s.reset);
    assert_eq!((s.inserted, s.skipped), (2, 0));
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 2)));

    // Round 2: one new mail (UID 3).
    account.add_message(
        "INBOX",
        mail(Some("grow-3@scenario.test"), "grow mail 3", "growbody3"),
    );
    let searches_before = account.server.log().searches.len();
    let fetches_before = account.server.log().fetch_sets.len();
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(!s.reset);
    assert_eq!(s.seen, 1);
    assert_eq!((s.inserted, s.skipped), (1, 0));

    // Round 3: three more (UIDs 4..=6).
    for i in 4..=6 {
        account.add_message(
            "INBOX",
            mail(
                Some(&format!("grow-{i}@scenario.test")),
                &format!("grow mail {i}"),
                &format!("growbody{i}"),
            ),
        );
    }
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(!s.reset);
    assert_eq!(s.seen, 3);
    assert_eq!(s.inserted, 3);

    // Server-side evidence for rounds 2 and 3: the searches ranged past
    // the cursor each time, and the fetch sets covered exactly the new
    // UIDs — never a refetch of 1..=2 or 1..=3.
    let log = account.server.log();
    assert_eq!(
        &log.searches[searches_before..],
        &["3:*".to_string(), "4:*".to_string()]
    );
    assert_eq!(
        &log.fetch_sets[fetches_before..],
        &[uid_set([3u32]), uid_set(4..=6u32)]
    );

    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 6)));
    assert_eq!(storage.stats().unwrap().total, 6);
}

/// A quiet folder costs nothing: when new mail arrives only in Archive,
/// INBOX is examined but never searched or fetched (the UIDNEXT
/// short-circuit), and the round still reports a healthy summary for it.
#[test]
fn a_quiet_folder_is_not_searched_or_fetched() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(1, "quietin")));
    account.set_mailbox("Archive", MockMailbox::new(3, mails(1, "quietar")));

    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX", "Archive"]);
    let searches_before = account.server.log().searches.len();
    let fetches_before = account.server.log().fetch_sets.len();

    // New mail arrives in Archive only.
    account.add_message(
        "Archive",
        mail(
            Some("quietar-2@scenario.test"),
            "quiet archive 2",
            "quietarbody2",
        ),
    );
    let summaries = account.sync_round(&storage, &["INBOX", "Archive"]);

    // INBOX is still reported, healthy and empty-handed.
    let inbox = summary_for(&summaries, "INBOX");
    assert_eq!(inbox.error, None);
    assert_eq!(inbox.seen, 0);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 1)));

    // The server saw exactly one SEARCH and one FETCH this round — for
    // Archive. INBOX produced neither.
    let log = account.server.log();
    assert_eq!(log.searches.len() - searches_before, 1);
    assert_eq!(log.searches.last().unwrap(), "2:*");
    assert_eq!(log.fetch_sets.len() - fetches_before, 1);
    assert_eq!(log.fetch_sets.last().unwrap(), "2");

    let archive = summary_for(&summaries, "Archive");
    assert_eq!(archive.inserted, 1);
    assert_eq!(cursor(&storage, USER, "Archive"), Some((3, 2)));
    assert_eq!(storage.stats().unwrap().total, 3);
}

/// New mail in both folders in one round: each folder's cursor advances
/// independently and both fetches happen in the single round.
#[test]
fn mail_arriving_in_two_folders_in_one_round_is_all_picked_up() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "bothin")));
    account.set_mailbox("Work", MockMailbox::new(5, mails(2, "bothwork")));

    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX", "Work"]);
    assert_eq!(storage.stats().unwrap().total, 4);

    account.add_message(
        "INBOX",
        mail(Some("bothin-3@scenario.test"), "both in 3", "bothinbody3"),
    );
    account.add_message(
        "Work",
        mail(
            Some("bothwork-3@scenario.test"),
            "both work 3",
            "bothworkbody3",
        ),
    );

    let fetches_before = account.server.log().fetch_sets.len();
    let summaries = account.sync_round(&storage, &["INBOX", "Work"]);

    let inbox = summary_for(&summaries, "INBOX");
    let work = summary_for(&summaries, "Work");
    assert_eq!(
        (inbox.error.as_deref(), inbox.seen, inbox.inserted),
        (None, 1, 1)
    );
    assert_eq!(
        (work.error.as_deref(), work.seen, work.inserted),
        (None, 1, 1)
    );

    // Two fetches this round — one per folder with new mail — and both new
    // mails are individually findable.
    assert_eq!(account.server.log().fetch_sets.len() - fetches_before, 2);
    assert_eq!(crate::find(&storage, "bothinbody3").len(), 1);
    assert_eq!(crate::find(&storage, "bothworkbody3").len(), 1);

    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 3)));
    assert_eq!(cursor(&storage, USER, "Work"), Some((5, 3)));
    assert_eq!(storage.stats().unwrap().total, 6);
}
