//! Scenario: UIDVALIDITY reset — the server recreates a folder and
//! reassigns every UID. The local UID map (the per-folder sync cursor in
//! `imap_sync_state`) must adopt the new validity and rescan from UID 1,
//! while `(source, external_id)` dedup keeps the archive duplicate-free.

use crate::imap_mock::MockMailbox;
use crate::{cursor, find, mails, summary_for, Account, USER};
use serde_json::Value;
use vault_core::storage::Storage;

fn one_mail(id: &str, tag: &str) -> Vec<u8> {
    crate::mail(Some(id), &format!("{tag} subject"), &format!("{tag}body"))
}

/// The folder is recreated with the *same* mails at *different* UIDs plus
/// one new mail: the local map adopts the new validity, the rescan dedups
/// the old mails, the new mail is stored, and nothing is duplicated.
#[test]
fn reset_with_reordered_uids_dedups_and_moves_the_map_to_the_new_validity() {
    let account = Account::start(USER);
    let original = mails(3, "old");
    account.set_mailbox("INBOX", MockMailbox::new(7, original.clone()));

    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 3)));

    // The folder is recreated: UIDVALIDITY 7 → 99, the three mails
    // reattached in reverse order (every UID reassigned), plus one new
    // mail that lands at UID 4.
    let mut recreated = original;
    recreated.reverse();
    recreated.push(one_mail("new-1@scenario.test", "newone"));
    account.set_mailbox("INBOX", MockMailbox::new(99, recreated));

    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.uidvalidity, 99);
    assert!(
        s.reset,
        "a validity change must trigger a rescan from UID 1"
    );
    assert_eq!(s.seen, 4, "rescan covers all four UIDs under the new map");
    assert_eq!(s.skipped, 3, "the three known mails dedup, not duplicate");
    assert_eq!(s.inserted, 1, "only the genuinely new mail is stored");

    // The local UID map now tracks the new validity up to the new highest
    // UID — the old (7, 3) map is gone.
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((99, 4)));

    // The archive did not grow by duplicates, and the new mail carries the
    // *new* validity in its provenance.
    assert_eq!(storage.stats().unwrap().total, 4);
    let hits = find(&storage, "newonebody");
    assert_eq!(hits.len(), 1);
    let raw: Value = serde_json::from_str(&hits[0].raw_json).unwrap();
    assert_eq!(raw["folder"], "INBOX");
    assert_eq!(raw["uid"], 4);
    assert_eq!(raw["uidvalidity"], 99);
}

/// The folder is recreated *empty* (all mail deleted server-side). The map
/// moves to the new validity, the archive keeps its history, and mail
/// arriving under the new validity is fetched incrementally from UID 1.
#[test]
fn reset_to_an_empty_folder_moves_the_map_and_keeps_the_archive() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "gone")));

    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);
    assert_eq!(storage.stats().unwrap().total, 3);

    // Recreated empty under a new validity.
    account.set_mailbox("INBOX", MockMailbox::new(8, Vec::new()));
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert!(s.reset);
    assert_eq!(s.seen, 0, "an empty folder has nothing to rescan");

    // Map adopted the new validity with a zero cursor; the archive kept
    // every mail it had already stored (the vault is an archive, not a
    // mirror).
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((8, 0)));
    assert_eq!(storage.stats().unwrap().total, 3);

    // Mail arriving under the new validity starts from UID 1.
    account.add_message("INBOX", one_mail("fresh-1@scenario.test", "fresh"));
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert!(!s.reset, "validity 8 is now the known one");
    assert_eq!(s.seen, 1);
    assert_eq!(s.inserted, 1);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((8, 1)));
    assert_eq!(storage.stats().unwrap().total, 4);
}

/// The folder is recreated holding *only brand-new mail* (everything old
/// is gone server-side): the rescan stores the new mail, and the old rows
/// stay archived under their original provenance.
#[test]
fn reset_whose_rescan_finds_only_new_mail_leaves_old_rows_untouched() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "old")));
    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);

    account.set_mailbox(
        "INBOX",
        MockMailbox::new(9, vec![one_mail("only-1@scenario.test", "onlynew")]),
    );
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.uidvalidity, 9);
    assert!(s.reset);
    assert_eq!(s.seen, 1);
    assert_eq!(s.skipped, 0, "the old mails are not on the server anymore");
    assert_eq!(s.inserted, 1);

    // The archive keeps the three old mails alongside the new one.
    assert_eq!(storage.stats().unwrap().total, 4);
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((9, 1)));

    // And the next round is quiet, not another rescan.
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(s.error, None);
    assert!(!s.reset);
    assert_eq!(s.seen, 0);
}

/// Full lifecycle across two resets: validity 7 → 42 → 100, with new mail
/// in each era. Each reset rescans and dedups; each era's new mail lands;
/// the map ends at the last validity with the era's highest UID.
#[test]
fn two_successive_resets_each_adopted_by_the_local_map() {
    let account = Account::start(USER);
    let era1 = mails(2, "era1");
    account.set_mailbox("INBOX", MockMailbox::new(7, era1.clone()));
    let storage = Storage::open_in_memory().unwrap();
    account.sync_round(&storage, &["INBOX"]);

    // Reset 1: same two mails plus one new, validity 42.
    let mut era2 = era1.clone();
    era2.push(one_mail("era2-1@scenario.test", "era2new"));
    account.set_mailbox("INBOX", MockMailbox::new(42, era2));
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(
        (s.uidvalidity, s.seen, s.skipped, s.inserted),
        (42, 3, 2, 1)
    );
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((42, 3)));

    // Reset 2: every mail archived so far plus one more new mail, validity 100.
    let mut era3 = era1.clone(); // the two era-1 mails
    era3.push(one_mail("era2-1@scenario.test", "era2new")); // the era-2 addition
    era3.push(one_mail("era3-1@scenario.test", "era3new")); // new in era 3
    account.set_mailbox("INBOX", MockMailbox::new(100, era3));
    let summaries = account.sync_round(&storage, &["INBOX"]);
    let s = summary_for(&summaries, "INBOX");
    assert_eq!(
        (s.uidvalidity, s.seen, s.skipped, s.inserted),
        (100, 4, 3, 1),
        "era 3 rescans the three archived mails and stores its one new mail"
    );
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((100, 4)));
    assert_eq!(
        storage.stats().unwrap().total,
        4,
        "no duplicates across two resets"
    );
}
