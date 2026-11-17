//! Collections backend (migration 19, part 1 of the collections slice):
//! vault-local sets of references to canonical messages. The contracts
//! proven here mirror the slice's acceptance behavior —
//!
//! - one message may belong to many collections across source instances
//!   without duplicating content;
//! - collection deletion removes organization only, never the archive;
//! - reimporting an already-archived message preserves memberships;
//! - an existing vault from the previous schema version reopens through
//!   the new migration with its organization intact;
//! - a backup/restore round-trip preserves memberships;
//! - a membership whose message is absent surfaces as unavailable
//!   rather than vanishing.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use vault_core::backup::{self, RestoreOptions};
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::{
    Collection, CollectionDetail, Storage, VaultFingerprint, SUPPORTED_SCHEMA_VERSION,
};

fn msg(source: &str, external_id: &str, body: &str) -> NewMessage {
    NewMessage::new(source, external_id, "ana", "2026-11-01T09:00:00Z", body)
}

/// A chat-shaped message (channel/chat grouping/conversation key set), so
/// collection detail rows exercise the conversation provenance join too.
fn chat_msg(source: &str, external_id: &str, body: &str) -> NewMessage {
    let mut m = msg(source, external_id, body);
    m.channel = "chat".into();
    m.chat_id = "renovation crew".into();
    m.conversation_key = "telegram:chat:424242".into();
    m
}

fn inserted_id(s: &Storage, msg: &NewMessage) -> i64 {
    match s.insert_message(msg).unwrap() {
        InsertOutcome::Inserted(id) | InsertOutcome::Duplicate(id) => id,
    }
}

fn names(collections: &[Collection]) -> Vec<&str> {
    collections.iter().map(|c| c.name.as_str()).collect()
}

fn member_ids(detail: &CollectionDetail) -> Vec<i64> {
    detail.members.iter().map(|m| m.message_id).collect()
}

#[test]
fn collections_create_rename_delete_and_list() {
    let s = Storage::open_in_memory().unwrap();

    // Names are trimmed; an empty name is refused for create and rename.
    let first = s.create_collection("  Apartment renovation  ").unwrap();
    assert_eq!(first.name, "Apartment renovation");
    assert_eq!(first.message_count, 0);
    assert!(!first.id.is_empty());
    assert!(s.create_collection("   ").is_err());
    assert!(s.create_collection("").is_err());

    let _second = s.create_collection("Taxes").unwrap();

    // Rename preserves the id and refreshes updated_at.
    let second = s.list_collections().unwrap()[1].clone();
    let before = second.updated_at.clone();
    let old_id = second.id.clone();
    assert!(s.rename_collection(&old_id, "  Taxes 2026 ").unwrap());
    let renamed = s
        .list_collections()
        .unwrap()
        .into_iter()
        .find(|c| c.id == old_id)
        .unwrap();
    assert_eq!(renamed.name, "Taxes 2026");
    assert!(renamed.updated_at >= before);
    assert!(!s.rename_collection("no-such-id", "X").unwrap());
    assert!(s.rename_collection(&old_id, " ").is_err());

    // Listing is oldest first.
    assert_eq!(
        names(&s.list_collections().unwrap()),
        ["Apartment renovation", "Taxes 2026"]
    );

    // Delete removes the collection (and only it); a second delete is
    // a no-op.
    assert!(s.delete_collection(&first.id).unwrap());
    assert!(!s.delete_collection(&first.id).unwrap());
    assert_eq!(names(&s.list_collections().unwrap()), ["Taxes 2026"]);
    assert!(s.collection_detail(&first.id).unwrap().is_none());
}

#[test]
fn one_message_joins_collections_across_sources_without_duplicating_content() {
    let s = Storage::open_in_memory().unwrap();
    let mail = inserted_id(&s, &msg("email-a", "m1", "quote for the tiles"));
    let chat = inserted_id(
        &s,
        &chat_msg(
            "telegram-export",
            "424242/8",
            "painter confirmed for monday",
        ),
    );
    let archive_total = s.stats().unwrap().total;

    let renovation = s.create_collection("Apartment renovation").unwrap();
    let quotes = s.create_collection("Quotes").unwrap();
    assert_eq!(
        s.add_collection_members(&renovation.id, &[mail, chat])
            .unwrap(),
        2
    );
    assert_eq!(s.add_collection_members(&quotes.id, &[mail]).unwrap(), 1);

    // Both collections list the mail message; no content moved: the
    // archive row count is unchanged and each detail row carries the
    // *reference* plus provenance, never a copy of the message.
    assert_eq!(s.stats().unwrap().total, archive_total);
    let listed = s.list_collections().unwrap();
    assert_eq!(
        listed.iter().map(|c| c.message_count).collect::<Vec<_>>(),
        vec![2, 1]
    );

    // The shared message is a member of both collections at once.
    for collection_id in [&renovation.id, &quotes.id] {
        let detail = s.collection_detail(collection_id).unwrap().unwrap();
        assert!(member_ids(&detail).contains(&mail));
    }

    // Detail rows carry the provenance the v2 detail path joins:
    // distinct source labels for the two source instances, the chat
    // row's conversation, and the resolved instant.
    let detail = s.collection_detail(&renovation.id).unwrap().unwrap();
    let mail_row = detail
        .members
        .iter()
        .find(|m| m.message_id == mail)
        .unwrap();
    assert!(mail_row.available);
    assert_eq!(mail_row.source_label.as_deref(), Some("email-a"));
    assert_eq!(mail_row.sent_at.as_deref(), Some("2026-11-01T09:00:00Z"));
    let chat_row = detail
        .members
        .iter()
        .find(|m| m.message_id == chat)
        .unwrap();
    assert_eq!(chat_row.source_label.as_deref(), Some("telegram-export"));
    let conversation_id = chat_row.conversation_id.clone();
    assert!(conversation_id.is_some());
    assert_eq!(
        chat_row.conversation_label.as_deref(),
        Some("renovation crew")
    );

    // A member can be removed from one collection while the other — and
    // the archive row — keep it.
    assert!(s.remove_collection_member(&quotes.id, mail).unwrap());
    let quotes_detail = s.collection_detail(&quotes.id).unwrap().unwrap();
    assert!(quotes_detail.members.is_empty());
    assert!(s.get(mail).unwrap().is_some(), "the archive row stays");
    let renovation_detail = s.collection_detail(&renovation.id).unwrap().unwrap();
    assert!(member_ids(&renovation_detail).contains(&mail));
}

#[test]
fn membership_adds_are_idempotent_and_refuse_unresolvable_references() {
    let s = Storage::open_in_memory().unwrap();
    let id = inserted_id(&s, &msg("email-a", "m1", "body"));
    let c = s.create_collection("Apartment renovation").unwrap();

    // Re-adding an existing membership changes nothing.
    assert_eq!(s.add_collection_members(&c.id, &[id]).unwrap(), 1);
    assert_eq!(s.add_collection_members(&c.id, &[id]).unwrap(), 0);
    // Duplicate ids within one call add one membership.
    assert_eq!(s.add_collection_members(&c.id, &[id, id]).unwrap(), 0);

    // A message that is not in the archive is refused, not stored as an
    // unavailable reference (absence surfaces only after the fact).
    let err = s.add_collection_members(&c.id, &[id, 999999]).unwrap_err();
    assert!(
        matches!(err, vault_core::Error::InvalidCollection(_)),
        "{err}"
    );
    let detail = s.collection_detail(&c.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![id], "refused batch added nothing");

    // Unknown collection ids are refused even for an empty batch.
    let err = s
        .add_collection_members("no-such-collection", &[])
        .unwrap_err();
    assert!(
        matches!(err, vault_core::Error::InvalidCollection(_)),
        "{err}"
    );

    // Removing is idempotent: true once, then false.
    assert!(s.remove_collection_member(&c.id, id).unwrap());
    assert!(!s.remove_collection_member(&c.id, id).unwrap());
}

#[test]
fn collection_deletion_removes_organization_only() {
    let s = Storage::open_in_memory().unwrap();
    let mail = inserted_id(&s, &msg("email-a", "m1", "the floor plan attached"));
    let chat = inserted_id(
        &s,
        &chat_msg("telegram-export", "424242/9", "carpenter starts tuesday"),
    );
    let keep = s.create_collection("Keep").unwrap();
    let gone = s.create_collection("Delete me").unwrap();
    s.add_collection_members(&keep.id, &[mail]).unwrap();
    s.add_collection_members(&gone.id, &[mail, chat]).unwrap();

    assert!(s.delete_collection(&gone.id).unwrap());

    // The archive is untouched: rows, bodies, and FTS all still there.
    assert_eq!(s.stats().unwrap().total, 2);
    assert_eq!(
        s.get(mail).unwrap().unwrap().body,
        "the floor plan attached"
    );
    assert_eq!(
        s.get(chat).unwrap().unwrap().body,
        "carpenter starts tuesday"
    );
    assert_eq!(
        s.search_filtered("\"floor plan\"", None, None, None, false, 10)
            .unwrap()
            .len(),
        1
    );

    // The other collection's organization is untouched too.
    let listed = s.list_collections().unwrap();
    assert_eq!(names(&listed), ["Keep"]);
    assert_eq!(listed[0].message_count, 1);
    let detail = s.collection_detail(&keep.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![mail]);
}

#[test]
fn reimport_of_an_archived_message_preserves_memberships() {
    let s = Storage::open_in_memory().unwrap();
    let original = msg("email-a", "m1", "original body");
    let id = inserted_id(&s, &original);

    let renovation = s.create_collection("Apartment renovation").unwrap();
    let quotes = s.create_collection("Quotes").unwrap();
    s.add_collection_members(&renovation.id, &[id]).unwrap();
    s.add_collection_members(&quotes.id, &[id]).unwrap();

    // Reimport the same message (identity is source + external id; the
    // body even differs) — the duplicate resolves onto the existing row
    // id, so every membership survives by construction.
    let reimport = msg("email-a", "m1", "a body the source no longer matches");
    let outcome = s.insert_message(&reimport).unwrap();
    assert!(matches!(outcome, InsertOutcome::Duplicate(dup) if dup == id));

    assert_eq!(s.stats().unwrap().total, 1, "no duplicate archive row");
    for collection_id in [&renovation.id, &quotes.id] {
        let detail = s.collection_detail(collection_id).unwrap().unwrap();
        assert_eq!(member_ids(&detail), vec![id]);
        assert!(detail.members[0].available);
    }
    assert_eq!(
        s.list_collections()
            .unwrap()
            .iter()
            .map(|c| c.message_count)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );

    // And the stored body stays the first-write original.
    assert_eq!(s.get(id).unwrap().unwrap().body, "original body");
}

/// Build a database in the exact shape the previous release (schema
/// version 17) left behind: full current schema minus the post-17
/// migrations' tables (collections 19; people/identity links 20 — and,
/// once the saved-searches slice lands, 18), their bookkeeping rows
/// absent. Reopening it with this build is the "existing vault upgrades
/// in place" path. Returns the archived message's row id.
fn version17_db(db: &std::path::Path) -> i64 {
    let s = Storage::open(db).unwrap();
    let id = inserted_id(
        &s,
        &msg("email-a", "pre-migration", "archived before collections"),
    );
    drop(s);

    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        "DROP TABLE collection_members;
         DROP TABLE collections;
         DROP TABLE person_identity_links;
         DROP TABLE people;
         DROP TABLE saved_searches;
         DROP TABLE IF EXISTS attachment_refs;
         DROP INDEX IF EXISTS idx_archive_messages_created_source;
         DELETE FROM schema_migrations WHERE version >= 18;",
    )
    .unwrap();
    id
}

#[test]
fn reopen_after_migration_keeps_the_archive_and_carries_organization_forward() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let pre = version17_db(&db);

    // Reopening with the current build applies the pending migrations
    // (19 through the safe-migration snapshot path, 20 after it) and
    // the archive survives.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), SUPPORTED_SCHEMA_VERSION);
    assert_eq!(s.stats().unwrap().total, 1);
    assert_eq!(
        s.get(pre).unwrap().unwrap().body,
        "archived before collections"
    );

    // The upgraded schema is immediately functional.
    let id = inserted_id(
        &s,
        &msg("email-a", "post-migration", "archived after the upgrade"),
    );
    let renovation = s.create_collection("Apartment renovation").unwrap();
    s.add_collection_members(&renovation.id, &[id]).unwrap();
    let detail = s.collection_detail(&renovation.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![id]);
    drop(s);

    // A further reopen applies nothing new and keeps the organization.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), SUPPORTED_SCHEMA_VERSION);
    let detail = s.collection_detail(&renovation.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![id]);
    assert!(detail.members[0].available);
}

#[test]
fn an_absent_membership_message_surfaces_as_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let s = Storage::open(&db).unwrap();
    let keep = inserted_id(&s, &msg("email-a", "keep", "this row survives"));
    let lose = inserted_id(&s, &msg("email-a", "lose", "this row disappears"));
    let c = s.create_collection("Apartment renovation").unwrap();
    s.add_collection_members(&c.id, &[keep, lose]).unwrap();
    drop(s);

    // Simulate the canonical row going absent underneath the vault (the
    // app itself never deletes archive rows; raw SQL stands in for a
    // vault whose archive changed under its organization layer).
    let conn = Connection::open(&db).unwrap();
    conn.execute("DELETE FROM archive_messages WHERE id = ?1", [lose])
        .unwrap();
    drop(conn);

    let s = Storage::open(&db).unwrap();
    let detail = s.collection_detail(&c.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![keep, lose], "membership stays");

    let missing = detail
        .members
        .iter()
        .find(|m| m.message_id == lose)
        .unwrap();
    assert!(!missing.available);
    assert_eq!(missing.source_label, None);
    assert_eq!(missing.source_id, None);
    assert_eq!(missing.conversation_id, None);
    assert_eq!(missing.conversation_label, None);
    assert_eq!(missing.author, None);
    assert_eq!(missing.subject, None);
    assert_eq!(missing.sent_at, None);

    let present = detail
        .members
        .iter()
        .find(|m| m.message_id == keep)
        .unwrap();
    assert!(present.available);
    assert_eq!(present.source_label.as_deref(), Some("email-a"));

    // The list count still reports both memberships — nothing vanished.
    assert_eq!(s.list_collections().unwrap()[0].message_count, 2);

    // The unavailable membership can still be removed explicitly.
    assert!(s.remove_collection_member(&c.id, lose).unwrap());
    let detail = s.collection_detail(&c.id).unwrap().unwrap();
    assert_eq!(member_ids(&detail), vec![keep]);
}

/// One temp workspace shaped like the backup suite's: a `vault/` data
/// directory plus scratch space for the container.
struct Sandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join("collections-backup");
        fs::create_dir_all(root.join("vault")).unwrap();
        Self { keeper, root }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn db(&self) -> PathBuf {
        self.data_dir().join("vault.db")
    }

    fn with_storage<R>(&self, op: impl FnOnce(&Storage) -> R) -> R {
        let s = Storage::open(self.db()).unwrap();
        let result = op(&s);
        drop(s);
        result
    }
}

#[test]
fn backup_restore_round_trips_memberships() {
    let sb = Sandbox::new();

    // Seed: two sources, two collections, cross-source memberships.
    sb.with_storage(|s| {
        let mail = inserted_id(s, &msg("email-a", "m1", "the glazier quote"));
        let chat = inserted_id(
            s,
            &chat_msg("telegram-export", "424242/7", "window frames ordered"),
        );
        let renovation = s.create_collection("Apartment renovation").unwrap();
        let quotes = s.create_collection("Quotes").unwrap();
        s.add_collection_members(&renovation.id, &[mail, chat])
            .unwrap();
        s.add_collection_members(&quotes.id, &[mail]).unwrap();
    });

    let fingerprint: VaultFingerprint = sb.with_storage(|s| s.vault_fingerprint().unwrap());
    let dest = sb.root.join("backup.mcbackup");
    sb.with_storage(|s| {
        backup::create_backup(
            &sb.data_dir(),
            s.connection(),
            &dest,
            &|_| {},
            &|| false,
            &backup::BackupOptions::default(),
        )
        .unwrap();
    });

    // Mutate the organization after the backup: rename one collection,
    // drop one membership, add another collection. Restore must bring
    // the backup-time organization back wholesale.
    sb.with_storage(|s| {
        let listed = s.list_collections().unwrap();
        assert_eq!(listed.len(), 2);
        s.rename_collection(&listed[1].id, "Renamed after backup")
            .unwrap();
        s.remove_collection_member(&listed[0].id, listed_members(s, &listed[0].id)[0])
            .unwrap();
        s.create_collection("Added after backup").unwrap();
    });

    backup::restore_backup(
        &sb.data_dir(),
        &dest,
        &fingerprint,
        &|_| {},
        &RestoreOptions::default(),
        &|_| false,
    )
    .unwrap();

    sb.with_storage(|s| {
        assert_eq!(s.stats().unwrap().total, 2, "archive round-trips");
        let listed = s.list_collections().unwrap();
        assert_eq!(names(&listed), ["Apartment renovation", "Quotes"]);

        let renovation = &listed[0];
        assert_eq!(renovation.message_count, 2);
        let detail = s.collection_detail(&renovation.id).unwrap().unwrap();
        assert!(detail.members.iter().all(|m| m.available));
        assert_eq!(
            detail
                .members
                .iter()
                .filter(|m| m.source_label.as_deref() == Some("email-a"))
                .count(),
            1
        );
        assert_eq!(
            detail
                .members
                .iter()
                .filter(|m| m.source_label.as_deref() == Some("telegram-export"))
                .count(),
            1
        );

        let quotes = &listed[1];
        assert_eq!(quotes.name, "Quotes", "post-backup rename is undone");
        assert_eq!(quotes.message_count, 1);
    });
}

fn listed_members(s: &Storage, collection_id: &str) -> Vec<i64> {
    s.collection_detail(collection_id)
        .unwrap()
        .expect("collection exists")
        .members
        .iter()
        .map(|m| m.message_id)
        .collect()
}
