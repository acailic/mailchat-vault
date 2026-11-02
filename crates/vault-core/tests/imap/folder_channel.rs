//! Scenario: folder → channel mapping — each IMAP folder archives as its
//! own `channel`, with per-message folder/UID/UIDVALIDITY provenance in
//! `raw_json`, per-folder cursors, and source scoping that keeps accounts
//! separate.

use crate::imap_mock::MockMailbox;
use crate::{cursor, find, mails, summary_for, Account, OTHER_USER, USER};
use serde_json::Value;
use vault_core::storage::Storage;

/// A three-folder account (including a folder name with a space): every
/// stored message carries its folder as the channel and its per-folder
/// UID provenance, and each folder keeps its own cursor.
#[test]
fn every_folder_becomes_its_own_channel_with_uid_provenance() {
    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "chanin")));
    account.set_mailbox("Sent Items", MockMailbox::new(21, mails(1, "chansent")));
    account.set_mailbox("Archive", MockMailbox::new(3, mails(1, "chanar")));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = account.sync_round(&storage, &["INBOX", "Sent Items", "Archive"]);
    assert_eq!(summaries.len(), 3);
    for s in &summaries {
        assert_eq!(s.error, None, "{} must sync cleanly", s.folder);
    }

    // Each mail's channel is exactly its folder — including the spaced
    // name — and raw_json keeps the folder/uid/uidvalidity provenance.
    let first = &find(&storage, "chaninbody1")[0];
    assert_eq!(first.channel, "INBOX");
    assert_eq!(first.source, crate::config_for(USER).display_label(), "connector source renders as the human-readable label, never the raw instance id");
    assert_eq!(first.external_id, "chanin-1@scenario.test");
    let raw: Value = serde_json::from_str(&first.raw_json).unwrap();
    assert_eq!(raw["folder"], "INBOX");
    assert_eq!(raw["uid"], 1);
    assert_eq!(raw["uidvalidity"], 7);

    let second = &find(&storage, "chaninbody2")[0];
    assert_eq!(second.channel, "INBOX");
    let raw: Value = serde_json::from_str(&second.raw_json).unwrap();
    assert_eq!(raw["uid"], 2, "UIDs within the folder stay per-message");

    let sent = &find(&storage, "chansentbody1")[0];
    assert_eq!(sent.channel, "Sent Items");
    assert_eq!(sent.external_id, "chansent-1@scenario.test");

    let archive = &find(&storage, "chanarbody1")[0];
    assert_eq!(archive.channel, "Archive");

    // One cursor per folder, each under the account's source.
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 2)));
    assert_eq!(cursor(&storage, USER, "Sent Items"), Some((21, 1)));
    assert_eq!(cursor(&storage, USER, "Archive"), Some((3, 1)));
    assert_eq!(storage.stats().unwrap().total, 4);
}

/// Two accounts (two logins against the same server — the production
/// model would be different servers, but the scoping under test —
/// per-username durable source instances — is identical): the same mail
/// delivered to both is archived once *per account*, both keep the folder
/// as channel, and the cursors are independent per account.
#[test]
fn two_accounts_archive_the_same_mail_independently() {
    let account = Account::start(USER);
    account.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![crate::mail(
                Some("shared@scenario.test"),
                "Shared thread",
                "sharedbody",
            )],
        ),
    );

    let storage = Storage::open_in_memory().unwrap();
    let alice_summaries = account.sync_round(&storage, &["INBOX"]);
    let bob_summaries = account.sync_round_as(OTHER_USER, &storage, &["INBOX"]);

    for s in [&alice_summaries[0], &bob_summaries[0]] {
        assert_eq!(s.error, None);
        assert_eq!(s.inserted, 1, "each account stores the mail once");
    }

    // Two rows — one per account's durable instance — and each account is
    // its own source in the stats.
    assert_eq!(storage.stats().unwrap().total, 2);
    assert_eq!(storage.stats().unwrap().by_source.len(), 2);

    let hits = find(&storage, "sharedbody");
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|m| m.channel == "INBOX"));
    let mut sources: Vec<String> = hits.iter().map(|m| m.source.clone()).collect();
    sources.sort();
    let mut expected = vec![
        crate::config_for(USER).display_label(),
        crate::config_for(OTHER_USER).display_label(),
    ];
    expected.sort();
    assert_eq!(
        sources, expected,
        "the two rows belong to the two accounts' instances"
    );

    // Independent cursors under each account's source.
    assert_eq!(cursor(&storage, USER, "INBOX"), Some((7, 1)));
    assert_eq!(cursor(&storage, OTHER_USER, "INBOX"), Some((7, 1)));

    // A resync by either account is a no-op: no cross-account cursor
    // confusion, no duplicates.
    let summaries = account.sync_round(&storage, &["INBOX"]);
    assert_eq!(summary_for(&summaries, "INBOX").seen, 0);
    let summaries = account.sync_round_as(OTHER_USER, &storage, &["INBOX"]);
    assert_eq!(summary_for(&summaries, "INBOX").seen, 0);
    assert_eq!(storage.stats().unwrap().total, 2);
}
