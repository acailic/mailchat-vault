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
//! - people and links persist in the vault and survive a reopen.

use vault_core::message::NewMessage;
use vault_core::storage::{NewPersonIdentityLink, PersonAnchorKind, Storage};
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
