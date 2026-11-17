//! People and user-confirmed identity links (migration 20) — the
//! storage slice of the unified person view.
//!
//! The scenarios assert the identity contract from
//! docs/data-and-api-contracts.md directly against the vault:
//!
//! - person records are a pure navigation layer: create, rename, and
//!   delete never touch archived content;
//! - linking is the only way an identity joins a person — a shared
//!   display name across sources links nothing by itself;
//! - link and unlink are reversible, and one identity belongs to at
//!   most one person, with conflicting assignments refused with an
//!   explanation instead of silently moving the link;
//! - people and links persist in the vault and survive a reopen and a
//!   backup/restore round trip.
//!
//! The A5a scenarios below cover the interaction contract on top:
//! preview/commit with an organization revision, one-transaction
//! commits that roll back on refusal, the history ledger (migration
//! 23), and same-label ambiguity staying distinct.

use vault_core::backup::{self, BackupOptions, RestoreOptions};
use vault_core::message::NewMessage;
use vault_core::storage::{
    NewPersonIdentityLink, PersonAnchorKind, PersonLinkOp, PersonLinkOperation, Storage,
};
use vault_core::Error;

/// The canonical source-instance id a legacy import under `namespace`
/// resolves to (inserting one message first makes the instance exist).
/// The legacy label defaults to the namespace, which stays unique per
/// instance here.
fn instance_for(storage: &Storage, namespace: &str) -> String {
    storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == "legacy" && i.label == namespace)
        .unwrap_or_else(|| panic!("no source instance for namespace {namespace}"))
        .id
}

fn insert_one(storage: &Storage, source: &str, external_id: &str, author: &str, body: &str) -> i64 {
    match storage
        .insert_message(&NewMessage::new(
            source,
            external_id,
            author,
            "2026-11-01T10:00:00Z",
            body,
        ))
        .unwrap()
    {
        vault_core::message::InsertOutcome::Inserted(id) => id,
        vault_core::message::InsertOutcome::Duplicate(id) => id,
    }
}

fn mail_link<'a>(instance: &'a str, anchor_value: &'a str) -> NewPersonIdentityLink<'a> {
    NewPersonIdentityLink {
        source_instance_id: instance,
        anchor_kind: PersonAnchorKind::StableKey,
        anchor_value,
        display_label: anchor_value,
    }
}

fn name_link<'a>(instance: &'a str, author: &'a str) -> NewPersonIdentityLink<'a> {
    NewPersonIdentityLink {
        source_instance_id: instance,
        anchor_kind: PersonAnchorKind::AuthorName,
        anchor_value: author,
        display_label: author,
    }
}

// ---------------------------------------------------------------------
// Person records
// ---------------------------------------------------------------------

/// Create, rename, and delete are pure association-layer operations:
/// messages inserted before, between, and after stay byte-identical.
#[test]
fn person_create_rename_delete_leaves_archived_content_unchanged() {
    let storage = Storage::open_in_memory().unwrap();
    let id_a = insert_one(
        &storage,
        "mbox",
        "a@vault",
        "Ana <ana@example.com>",
        "before",
    );
    insert_one(
        &storage,
        "mbox",
        "b@vault",
        "Ana <ana@example.com>",
        "after",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    assert_eq!(person.name, "Ana Ilic");
    assert_eq!(storage.list_people().unwrap(), vec![person.clone()]);

    // A blank name is invalid input, not a person.
    assert!(matches!(
        storage.create_person("   "),
        Err(Error::InvalidPersonInput(_))
    ));

    storage
        .rename_person(&person.id, "Ana Ilic-Perišić")
        .unwrap();
    let renamed = &storage.list_people().unwrap()[0];
    assert_eq!(renamed.name, "Ana Ilic-Perišić");
    assert_eq!(renamed.id, person.id);
    assert!(renamed.updated_at >= renamed.created_at);

    assert!(matches!(
        storage.rename_person("no-such-person", "X"),
        Err(Error::PersonNotFound(_))
    ));

    storage.delete_person(&person.id).unwrap();
    assert!(storage.list_people().unwrap().is_empty());
    assert!(matches!(
        storage.delete_person(&person.id),
        Err(Error::PersonNotFound(_))
    ));

    // The archived rows are untouched by every person operation.
    for (id, body) in [(id_a, "before"), (id_a + 1, "after")] {
        let msg = storage.get(id).unwrap().unwrap();
        assert_eq!(msg.body, body);
        assert_eq!(msg.author, "Ana <ana@example.com>");
    }
}

// ---------------------------------------------------------------------
// Linking and unlinking
// ---------------------------------------------------------------------

/// Link and unlink are reversible, and neither direction changes any
/// archived row.
#[test]
fn link_and_unlink_are_reversible_with_archived_content_unchanged() {
    let storage = Storage::open_in_memory().unwrap();
    let mid = insert_one(
        &storage,
        "mbox",
        "a@vault",
        "Ana <ana@example.com>",
        "hello",
    );

    let person = storage.create_person("Ana").unwrap();
    let instance = instance_for(&storage, "mbox");
    storage
        .link_person_identity(&person.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();

    // The anchor resolves to the person while linked.
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );
    // Stable keys are case-folded: the lookup folds the same way.
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ANA@Example.COM")
            .unwrap()
            .as_ref(),
        Some(&person)
    );

    let links = storage.person_identity_links(&person.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].anchor_value, "ana@example.com");
    assert_eq!(links[0].anchor_kind, PersonAnchorKind::StableKey);

    // Unlink removes exactly the association row.
    storage
        .unlink_person_identity(&person.id, &links[0].id)
        .unwrap();
    assert!(storage
        .person_identity_links(&person.id)
        .unwrap()
        .is_empty());
    assert!(storage
        .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
        .unwrap()
        .is_none());
    assert!(matches!(
        storage.unlink_person_identity(&person.id, &links[0].id),
        Err(Error::IdentityLinkNotFound(_))
    ));

    // Re-linking after an unlink works — the operation is reversible.
    storage
        .link_person_identity(&person.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );

    // The archived message never changed.
    let msg = storage.get(mid).unwrap().unwrap();
    assert_eq!(msg.author, "Ana <ana@example.com>");
    assert_eq!(msg.body, "hello");
    assert_eq!(storage.stats().unwrap().total, 1);
}

/// One person holds multiple aliases — here a mail address (stable
/// key) and a chat display name (author-name anchor) — and each
/// resolves back to the same person.
#[test]
fn one_person_holds_multiple_aliases() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "mail body");
    insert_one(&storage, "telegram-work", "t1", "Ana", "chat body");

    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    let chat = instance_for(&storage, "telegram-work");

    storage
        .link_person_identity(&person.id, &mail_link(&mail, "ana@example.com"))
        .unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&chat, "Ana"))
        .unwrap();

    let links = storage.person_identity_links(&person.id).unwrap();
    assert_eq!(links.len(), 2, "both aliases live on one person");

    for (instance, kind, value) in [
        (&mail, PersonAnchorKind::StableKey, "ana@example.com"),
        (&chat, PersonAnchorKind::AuthorName, "Ana"),
    ] {
        assert_eq!(
            storage
                .person_for_anchor(instance, kind, value)
                .unwrap()
                .as_ref(),
            Some(&person),
            "alias {value:?} must resolve to the one person"
        );
    }
}

/// The same display name on two different sources is two different
/// identities: linking one never links the other, and each can belong
/// to a different person.
#[test]
fn same_display_name_on_different_sources_never_links_without_an_explicit_call() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "telegram-work", "t1", "Bob", "work chat");
    insert_one(&storage, "telegram-family", "f1", "Bob", "family chat");

    let work = instance_for(&storage, "telegram-work");
    let family = instance_for(&storage, "telegram-family");
    assert_ne!(work, family, "the two exports are distinct instances");

    let bob_boss = storage.create_person("Bob (colleague)").unwrap();
    storage
        .link_person_identity(&bob_boss.id, &name_link(&work, "Bob"))
        .unwrap();

    // The other source's "Bob" is untouched: no automatic linking.
    assert!(
        storage
            .person_for_anchor(&family, PersonAnchorKind::AuthorName, "Bob")
            .unwrap()
            .is_none(),
        "a shared display name across sources must not leak a link"
    );

    // It stays linkable — to a *different* person, explicitly.
    let bob_cousin = storage.create_person("Bob (cousin)").unwrap();
    storage
        .link_person_identity(&bob_cousin.id, &name_link(&family, "Bob"))
        .unwrap();
    assert_eq!(
        storage
            .person_for_anchor(&work, PersonAnchorKind::AuthorName, "Bob")
            .unwrap()
            .as_ref(),
        Some(&bob_boss)
    );
    assert_eq!(
        storage
            .person_for_anchor(&family, PersonAnchorKind::AuthorName, "Bob")
            .unwrap()
            .as_ref(),
        Some(&bob_cousin)
    );

    // Author-name anchors keep their verbatim case: "Bob" and "bob"
    // are different author strings within one source.
    assert!(storage
        .person_for_anchor(&work, PersonAnchorKind::AuthorName, "bob")
        .unwrap()
        .is_none());
}

/// An identity belongs to at most one person: a second person's link
/// attempt is refused with an explanation naming the current owner,
/// and reassignment happens only through an explicit unlink.
#[test]
fn conflicting_assignment_is_refused_with_an_explanation() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "body");

    let instance = instance_for(&storage, "mbox");
    let ana = storage.create_person("Ana Ilic").unwrap();
    let impostor = storage.create_person("Someone Else").unwrap();

    storage
        .link_person_identity(&ana.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();

    // The conflicting assignment is refused, with the owner named.
    let err = storage
        .link_person_identity(&impostor.id, &mail_link(&instance, "ana@example.com"))
        .unwrap_err();
    match &err {
        Error::IdentityLinkConflict {
            owner_id,
            owner_name,
            anchor_value,
            ..
        } => {
            assert_eq!(owner_id, &ana.id);
            assert_eq!(owner_name, "Ana Ilic");
            assert_eq!(anchor_value, "ana@example.com");
        }
        other => panic!("expected IdentityLinkConflict, got {other:?}"),
    }
    assert!(
        err.to_string().contains("Ana Ilic"),
        "the explanation must name the current owner: {err}"
    );

    // The refusal moved nothing.
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&ana)
    );
    assert!(storage
        .person_identity_links(&impostor.id)
        .unwrap()
        .is_empty());

    // Reassignment is explicit: unlink from the owner, then link.
    let link_id = storage.person_identity_links(&ana.id).unwrap()[0]
        .id
        .clone();
    storage.unlink_person_identity(&ana.id, &link_id).unwrap();
    storage
        .link_person_identity(&impostor.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&impostor)
    );
}

/// Deleting a person removes the associations only — the links
/// cascade, the messages stay, and the anchor becomes linkable again.
#[test]
fn deleting_a_person_removes_associations_only() {
    let storage = Storage::open_in_memory().unwrap();
    let mid = insert_one(&storage, "telegram-work", "t1", "Ana", "chat body");
    let instance = instance_for(&storage, "telegram-work");

    let person = storage.create_person("Ana Ilic").unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&instance, "Ana"))
        .unwrap();

    storage.delete_person(&person.id).unwrap();
    assert!(storage.list_people().unwrap().is_empty());
    assert!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::AuthorName, "Ana")
            .unwrap()
            .is_none(),
        "the association went with the person"
    );
    // The cascade really removed the link rows (foreign keys are on),
    // not merely the person they pointed at.
    let orphaned: i64 = storage
        .connection()
        .query_row("SELECT COUNT(*) FROM person_identity_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(orphaned, 0, "no association row may outlive its person");

    // The archived message is untouched and the anchor is free again.
    let msg = storage.get(mid).unwrap().unwrap();
    assert_eq!(msg.author, "Ana");
    assert_eq!(msg.body, "chat body");
    let next = storage.create_person("Ana Ilic (recreated)").unwrap();
    storage
        .link_person_identity(&next.id, &name_link(&instance, "Ana"))
        .unwrap();
}

// ---------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------

/// People and links live in the vault database: they survive a reopen
/// (and therefore restart, backup, and restore).
#[test]
fn people_and_links_survive_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("vault.db");
    let instance;
    let person;
    {
        let storage = Storage::open(&db).unwrap();
        insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "body");
        instance = instance_for(&storage, "mbox");
        person = storage.create_person("Ana Ilic").unwrap();
        storage
            .link_person_identity(&person.id, &mail_link(&instance, "ana@example.com"))
            .unwrap();
    }

    let storage = Storage::open(&db).unwrap();
    assert_eq!(storage.list_people().unwrap(), vec![person.clone()]);
    let links = storage.person_identity_links(&person.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].source_instance_id, instance);
    assert_eq!(links[0].anchor_value, "ana@example.com");
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );
}

/// Associations ride the vault's backup container: after a
/// backup/restore round trip the person, the link, and the anchor
/// resolution are all back — the same persistence path as every other
/// vault table.
#[test]
fn associations_survive_backup_and_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("vault");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = data_dir.join("vault.db");
    let container = tmp.path().join("person-vault.mcbackup");

    let (instance, person) = {
        let storage = Storage::open(&db).unwrap();
        insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "body");
        let instance = instance_for(&storage, "mbox");
        let person = storage.create_person("Ana Ilic").unwrap();
        storage
            .link_person_identity(&person.id, &mail_link(&instance, "ana@example.com"))
            .unwrap();

        let summary = backup::create_backup(
            &data_dir,
            storage.connection(),
            &container,
            &|_| {},
            &|| false,
            &BackupOptions::default(),
        )
        .unwrap();
        assert_eq!(
            summary.manifest.schema_version,
            vault_core::storage::SUPPORTED_SCHEMA_VERSION
        );
        let fingerprint = storage.vault_fingerprint().unwrap();
        backup::restore_backup(
            &data_dir,
            &container,
            &fingerprint,
            &|_| {},
            &RestoreOptions::default(),
            &|_: &vault_core::storage::SourceInstance| false,
        )
        .unwrap();
        (instance, person)
    };

    let storage = Storage::open(&db).unwrap();
    assert_eq!(storage.list_people().unwrap(), vec![person.clone()]);
    let links = storage.person_identity_links(&person.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].anchor_value, "ana@example.com");
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );
}

/// Links anchor to the stable identity, not to message rows: imports
/// that arrive after the link resolve to the person automatically,
/// without any re-confirmation touching the archived content.
#[test]
fn new_imports_under_a_linked_stable_key_resolve_automatically() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(
        &storage,
        "mbox",
        "old-1",
        "Ana <ana@example.com>",
        "imported before the link",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    let instance = instance_for(&storage, "mbox");
    storage
        .link_person_identity(&person.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();

    // Future imports under the same stable key need no new link.
    insert_one(
        &storage,
        "mbox",
        "new-1",
        "Ana <ana@example.com>",
        "imported after the link",
    );
    insert_one(
        &storage,
        "mbox",
        "new-2",
        "ana@example.com",
        "different author casing",
    );
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ANA@example.com")
            .unwrap()
            .as_ref(),
        Some(&person),
        "the case-folded stable key resolves the same person"
    );
    assert_eq!(storage.stats().unwrap().total, 3);
    assert_eq!(
        storage.person_identity_links(&person.id).unwrap().len(),
        1,
        "no new link rows appeared — the anchor already covers them"
    );
}

// ---------------------------------------------------------------------
// Preview / commit with an organization revision (A5a)
// ---------------------------------------------------------------------

/// The full cycle: preview reports per-identity counts and
/// representative messages with the revision it was read at; commit
/// applies the set in one transaction and bumps the revision; a
/// second commit carrying the old revision is rejected with the
/// current one and applies nothing.
#[test]
fn preview_commit_then_stale_commit_is_rejected_with_the_current_revision() {
    let storage = Storage::open_in_memory().unwrap();
    let m1 = insert_one(
        &storage,
        "mbox",
        "m1",
        "Ana <ana@example.com>",
        "first message",
    );
    insert_one(
        &storage,
        "mbox",
        "m2",
        "ana@example.com",
        "second message, bare address",
    );
    insert_one(&storage, "mbox", "m3", "Bo <bo@example.com>", "other person");

    let person = storage.create_person("Ana Ilic").unwrap();
    let instance = instance_for(&storage, "mbox");
    assert_eq!(storage.organization_revision().unwrap(), 0);

    let ops = [PersonLinkOp::Link {
        person_id: &person.id,
        link: mail_link(&instance, "ana@example.com"),
    }];
    let preview = storage.person_link_preview(&ops).unwrap();
    assert_eq!(preview.organization_revision, 0);
    assert_eq!(preview.entries.len(), 1);
    let entry = &preview.entries[0];
    assert_eq!(entry.person_id, person.id);
    assert_eq!(entry.anchor_kind, PersonAnchorKind::StableKey);
    assert_eq!(entry.anchor_value, "ana@example.com");
    assert_eq!(entry.matched_messages, 2, "both author forms match");
    assert_eq!(
        entry.representative_messages.len(),
        2,
        "few matches are all representative"
    );
    assert_eq!(entry.representative_messages[0].id, m1);
    assert_eq!(entry.representative_messages[0].author, "Ana <ana@example.com>");
    assert!(entry.representative_messages[0].snippet.contains("first message"));
    assert!(entry.current_owner.is_none());
    // A preview writes nothing.
    assert!(storage.person_identity_links(&person.id).unwrap().is_empty());
    assert_eq!(storage.organization_revision().unwrap(), 0);

    let commit = storage.person_links_commit(0, &ops).unwrap();
    assert_eq!(commit.organization_revision, 1);
    assert_eq!(storage.organization_revision().unwrap(), 1);
    assert_eq!(
        storage
            .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
            .unwrap()
            .as_ref(),
        Some(&person)
    );
    let history = storage.person_link_history(&person.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].operation, PersonLinkOperation::Link);
    assert_eq!(history[0].anchor_value, "ana@example.com");
    assert_eq!(history[0].organization_revision, 1);

    // A second commit carrying the stale revision 0 is rejected with
    // the current revision — and applies nothing.
    let stale_ops = [PersonLinkOp::Link {
        person_id: &person.id,
        link: mail_link(&instance, "other@example.com"),
    }];
    let err = storage.person_links_commit(0, &stale_ops).unwrap_err();
    match &err {
        Error::PersonLinksStaleRevision { expected, current } => {
            assert_eq!(*expected, 0);
            assert_eq!(*current, 1);
        }
        other => panic!("expected PersonLinksStaleRevision, got {other:?}"),
    }
    assert!(
        err.to_string().contains('1'),
        "the error exposes the current revision: {err}"
    );
    assert_eq!(
        storage.person_identity_links(&person.id).unwrap().len(),
        1,
        "the stale commit applied nothing"
    );
    assert_eq!(storage.organization_revision().unwrap(), 1);

    // Re-previewed at the current revision, the same set commits.
    let fresh = storage.person_link_preview(&stale_ops).unwrap();
    assert_eq!(fresh.organization_revision, 1);
    storage
        .person_links_commit(fresh.organization_revision, &stale_ops)
        .unwrap();
    assert_eq!(storage.organization_revision().unwrap(), 2);
    assert_eq!(
        storage.person_identity_links(&person.id).unwrap().len(),
        2
    );
}

/// The stable-key preview matches only real address evidence: both
/// `Name <address>` and bare-address author forms fold to the anchor,
/// while display-only authors and lookalike addresses never match.
#[test]
fn preview_stable_key_matches_address_evidence_only() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(
        &storage,
        "mbox",
        "m1",
        "Ana <ana@example.com>",
        "bracketed form",
    );
    insert_one(&storage, "mbox", "m2", "ana@example.com", "bare form");
    insert_one(
        &storage,
        "mbox",
        "m3",
        "ANA <Ana@Example.COM>",
        "bracketed, different case",
    );
    // Display-only chat authors have no stable key...
    insert_one(&storage, "telegram-work", "t1", "Ana", "chat display name");
    // ...and a lookalike address is a different identity.
    insert_one(
        &storage,
        "mbox",
        "m4",
        "ana@example.com.co",
        "lookalike domain",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    let chat = instance_for(&storage, "telegram-work");

    let mail_preview = storage
        .person_link_preview(&[PersonLinkOp::Link {
            person_id: &person.id,
            link: mail_link(&mail, "ana@example.com"),
        }])
        .unwrap();
    assert_eq!(mail_preview.entries[0].matched_messages, 3);
    assert!(mail_preview.entries[0]
        .representative_messages
        .iter()
        .all(|m| m.id <= 3));

    let chat_preview = storage
        .person_link_preview(&[PersonLinkOp::Link {
            person_id: &person.id,
            link: mail_link(&chat, "ana@example.com"),
        }])
        .unwrap();
    assert_eq!(
        chat_preview.entries[0].matched_messages, 0,
        "a display-name chat author never matches a stable-key anchor"
    );

    // The author-name anchor stays verbatim and source-scoped: only
    // the exact "Ana" rows in the chat instance.
    let name_preview = storage
        .person_link_preview(&[PersonLinkOp::Link {
            person_id: &person.id,
            link: name_link(&chat, "Ana"),
        }])
        .unwrap();
    assert_eq!(name_preview.entries[0].matched_messages, 1);
    assert_eq!(name_preview.entries[0].representative_messages.len(), 1);
    assert_eq!(name_preview.entries[0].representative_messages[0].id, 4);
}

/// A wrong link is reversible through the commit path: preview the
/// unlink, commit it, and the prior state is restored without a single
/// archived row — message bodies or sender strings — changing.
#[test]
fn wrong_link_committed_then_unlinked_restores_the_prior_state() {
    let storage = Storage::open_in_memory().unwrap();
    let ids: Vec<i64> = ["one", "two", "three"]
        .iter()
        .enumerate()
        .map(|(i, body)| {
            insert_one(
                &storage,
                "mbox",
                &format!("m{i}"),
                "Ana <ana@example.com>",
                body,
            )
        })
        .collect();
    insert_one(&storage, "mbox", "mx", "Bo <bo@example.com>", "unrelated");

    let ana = storage.create_person("Ana Ilic").unwrap();
    let instance = instance_for(&storage, "mbox");
    storage
        .person_links_commit(
            0,
            &[PersonLinkOp::Link {
                person_id: &ana.id,
                link: mail_link(&instance, "ana@example.com"),
            }],
        )
        .unwrap();

    // Wrong link noticed: preview the unlink before committing it.
    let link_id = storage.person_identity_links(&ana.id).unwrap()[0]
        .id
        .clone();
    let unlink_ops = [PersonLinkOp::Unlink {
        person_id: &ana.id,
        link_id: &link_id,
    }];
    let preview = storage.person_link_preview(&unlink_ops).unwrap();
    assert_eq!(preview.entries.len(), 1);
    assert_eq!(preview.entries[0].anchor_value, "ana@example.com");
    assert_eq!(preview.entries[0].matched_messages, 3);

    let commit = storage
        .person_links_commit(preview.organization_revision, &unlink_ops)
        .unwrap();
    assert_eq!(commit.organization_revision, 2);

    // Prior state restored: the anchor is free and linkable again.
    assert!(storage.person_identity_links(&ana.id).unwrap().is_empty());
    assert!(storage
        .person_for_anchor(&instance, PersonAnchorKind::StableKey, "ana@example.com")
        .unwrap()
        .is_none());
    let next = storage.create_person("Ana Ilic (redo)").unwrap();
    storage
        .link_person_identity(&next.id, &mail_link(&instance, "ana@example.com"))
        .unwrap();

    // Message rows and sender strings untouched throughout.
    for (id, body) in ids.iter().zip(["one", "two", "three"]) {
        let msg = storage.get(*id).unwrap().unwrap();
        assert_eq!(msg.author, "Ana <ana@example.com>");
        assert_eq!(msg.body, body);
    }
    assert_eq!(storage.stats().unwrap().total, 4);

    // The audit ledger kept both sides of the mistake.
    let mut history = storage.person_link_history(&ana.id).unwrap();
    history.sort_by_key(|r| r.id);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].operation, PersonLinkOperation::Link);
    assert_eq!(history[1].operation, PersonLinkOperation::Unlink);
    assert_eq!(history[1].organization_revision, 2);
}

/// Two same-label identities ("Mom" x2) stay distinct through preview
/// and commit: one entry per source instance, linkable to different
/// persons, never merged.
#[test]
fn same_label_identities_stay_distinct_through_preview_and_commit() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "telegram-work", "w1", "Mom", "work chat");
    insert_one(&storage, "telegram-work", "w2", "Mom", "more work chat");
    insert_one(&storage, "telegram-family", "f1", "Mom", "family chat");

    let mom_work = storage.create_person("Mom (work)").unwrap();
    let mom_family = storage.create_person("Mom (family)").unwrap();
    let work = instance_for(&storage, "telegram-work");
    let family = instance_for(&storage, "telegram-family");

    let ops = [
        PersonLinkOp::Link {
            person_id: &mom_work.id,
            link: name_link(&work, "Mom"),
        },
        PersonLinkOp::Link {
            person_id: &mom_family.id,
            link: name_link(&family, "Mom"),
        },
    ];
    let preview = storage.person_link_preview(&ops).unwrap();
    assert_eq!(
        preview.entries.len(),
        2,
        "same label, two identities: one entry each, never merged"
    );
    assert_ne!(
        preview.entries[0].source_instance_id, preview.entries[1].source_instance_id
    );
    assert_eq!(preview.entries[0].matched_messages, 2);
    assert_eq!(preview.entries[1].matched_messages, 1);
    assert_eq!(preview.entries[0].anchor_value, "Mom");
    assert_eq!(preview.entries[1].anchor_value, "Mom");

    let commit = storage
        .person_links_commit(preview.organization_revision, &ops)
        .unwrap();
    assert_eq!(
        commit.organization_revision, 1,
        "one set, one revision bump"
    );
    assert_eq!(
        storage
            .person_for_anchor(&work, PersonAnchorKind::AuthorName, "Mom")
            .unwrap()
            .as_ref(),
        Some(&mom_work)
    );
    assert_eq!(
        storage
            .person_for_anchor(&family, PersonAnchorKind::AuthorName, "Mom")
            .unwrap()
            .as_ref(),
        Some(&mom_family)
    );
    assert_eq!(
        storage.person_identity_links(&mom_work.id).unwrap().len(),
        1
    );
    assert_eq!(
        storage.person_identity_links(&mom_family.id).unwrap().len(),
        1
    );
}

/// A conflicting anchor is forecast by the preview and refused by the
/// commit — one transaction, nothing applied.
#[test]
fn preview_forecasts_conflicts_and_commit_refuses_them_atomically() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "body");
    let instance = instance_for(&storage, "mbox");
    let ana = storage.create_person("Ana Ilic").unwrap();
    let impostor = storage.create_person("Someone Else").unwrap();
    storage
        .person_links_commit(
            0,
            &[PersonLinkOp::Link {
                person_id: &ana.id,
                link: mail_link(&instance, "ana@example.com"),
            }],
        )
        .unwrap();

    let ops = [
        PersonLinkOp::Link {
            person_id: &impostor.id,
            link: mail_link(&instance, "ana@example.com"),
        },
        PersonLinkOp::Link {
            person_id: &impostor.id,
            link: mail_link(&instance, "bo@example.com"),
        },
    ];
    let preview = storage.person_link_preview(&ops).unwrap();
    assert_eq!(
        preview.entries[0].current_owner.as_ref(),
        Some(&ana),
        "the preview names the current owner before any commit"
    );
    assert!(preview.entries[1].current_owner.is_none());

    let err = storage
        .person_links_commit(preview.organization_revision, &ops)
        .unwrap_err();
    assert!(matches!(err, Error::IdentityLinkConflict { .. }));
    // One transaction: the refusable set applied nothing, not even its
    // acceptable half, and the revision did not move.
    assert!(storage.person_identity_links(&impostor.id).unwrap().is_empty());
    assert_eq!(
        storage.person_identity_links(&ana.id).unwrap().len(),
        1
    );
    assert_eq!(storage.organization_revision().unwrap(), 1);
    assert!(storage.person_link_history(&impostor.id).unwrap().is_empty());
}

/// Deleting a person removes organization only: the audit ledger —
/// deliberately not a foreign key — survives as the record of what was
/// organized.
#[test]
fn person_deletion_removes_organization_only_and_keeps_the_audit_ledger() {
    let storage = Storage::open_in_memory().unwrap();
    insert_one(&storage, "mbox", "m1", "Ana <ana@example.com>", "body");
    let instance = instance_for(&storage, "mbox");
    let person = storage.create_person("Ana Ilic").unwrap();
    storage
        .person_links_commit(
            0,
            &[PersonLinkOp::Link {
                person_id: &person.id,
                link: mail_link(&instance, "ana@example.com"),
            }],
        )
        .unwrap();

    storage.delete_person(&person.id).unwrap();
    assert!(storage.list_people().unwrap().is_empty());
    let history = storage.person_link_history(&person.id).unwrap();
    assert_eq!(
        history.len(),
        1,
        "the ledger is an audit record, not a live association"
    );
    assert_eq!(history[0].operation, PersonLinkOperation::Link);
}
