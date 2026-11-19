//! Person timeline across mail and chat (the backend slice of the
//! unified person view): one person's messages from every identity
//! link — mail stable-key, WhatsApp author-name, Telegram author-name
//! — return as one interleaved, sent-time-ordered thread.
//!
//! The scenarios assert the ticket's acceptance directly against the
//! vault:
//!
//! - the three-source thread interleaves in sent-time order with
//!   per-row source identity, and stable-key matching folds author
//!   casing exactly like the link-preview path while lookalike
//!   addresses, display-only chat names under a stable-key anchor,
//!   and verbatim-cased author strings never leak in;
//! - long threads page through the opaque keyset cursor (no offset
//!   loading anywhere in the API), across the unknown-instant
//!   boundary and the equal-instant id tiebreak;
//! - header facts (first/last contact, totals, per-source counts) and
//!   the linked identities with their confirmed-versus-inferred state
//!   come from the same matched set as the rows.

use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::person_timeline::{
    PersonLinkMatchState, PersonTimelineRequest, PersonTimelineSourceCount,
};
use vault_core::storage::{
    NewPersonIdentityLink, PersonAnchorKind, PersonIdentityCandidatesRequest, Storage,
};
use vault_core::Error;

/// The canonical source-instance id a legacy import under `namespace`
/// resolves to (inserting one message first makes the instance exist).
fn instance_for(storage: &Storage, namespace: &str) -> String {
    storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == "legacy" && i.label == namespace)
        .unwrap_or_else(|| panic!("no source instance for namespace {namespace}"))
        .id
}

/// Insert one message with an explicit sent instant (the person_links
/// helper's shape, with the time as a parameter so the fixture can
/// interleave sources). An empty `sent_at` leaves the instant unknown.
fn insert_at(
    storage: &Storage,
    source: &str,
    external_id: &str,
    author: &str,
    sent_at: &str,
    body: &str,
) -> i64 {
    match storage
        .insert_message(&NewMessage::new(source, external_id, author, sent_at, body))
        .unwrap()
    {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Duplicate(id) => id,
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

/// Drain a person's timeline through the keyset cursor alone (the
/// load-more loop later slices ride on): pages of `limit`, no offsets,
/// stopping at the first page without a `next_cursor`.
fn drain_through_cursor(
    storage: &Storage,
    person_id: &str,
    limit: u32,
) -> Vec<vault_core::person_timeline::PersonTimelineMessage> {
    let mut collected = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = storage
            .person_timeline(&PersonTimelineRequest {
                person_id: person_id.to_string(),
                limit: Some(limit),
                cursor,
            })
            .unwrap();
        collected.extend(page.messages);
        assert_eq!(page.limit, limit);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return collected,
        }
    }
}

/// The acceptance fixture: one person with a mail stable-key link, a
/// WhatsApp author-name link, and a Telegram author-name link renders
/// one interleaved thread in sent-time order, with honest header
/// facts, per-source counts, and identity states.
#[test]
fn three_source_person_interleaves_into_one_sent_time_thread() {
    let storage = Storage::open_in_memory().unwrap();
    // Mail under the stable key `ana@example.com`: every author form
    // the anchor folds (bracketed, bare, mixed case) matches.
    let m_jan = insert_at(
        &storage,
        "mbox",
        "m1",
        "ANA <Ana@Example.COM>",
        "2026-01-15T12:00:00Z",
        "mail, folded author",
    );
    let m_mar = insert_at(
        &storage,
        "mbox",
        "m2",
        "Ana <ana@example.com>",
        "2026-03-02T09:00:00Z",
        "mail one",
    );
    let m_jun = insert_at(
        &storage,
        "mbox",
        "m3",
        "ana@example.com",
        "2026-06-01T08:00:00Z",
        "mail two, bare address",
    );
    // ...and the never-matches: another address, a lookalike domain.
    insert_at(
        &storage,
        "mbox",
        "m4",
        "Bo <bo@example.com>",
        "2026-05-05T05:05:05Z",
        "not ana",
    );
    insert_at(
        &storage,
        "mbox",
        "m5",
        "ana@example.com.co",
        "2026-04-01T04:00:00Z",
        "lookalike domain",
    );
    // WhatsApp under the verbatim author name "Ana": exact matches
    // only — the same display name in Telegram is a different
    // identity, and a lowercased "ana" is a different author string.
    let w_feb = insert_at(
        &storage,
        "whatsapp-family",
        "w1",
        "Ana",
        "2026-02-20T10:30:00Z",
        "whatsapp one",
    );
    let w_may = insert_at(
        &storage,
        "whatsapp-family",
        "w2",
        "Ana",
        "2026-05-10T16:45:00Z",
        "whatsapp two",
    );
    insert_at(
        &storage,
        "whatsapp-family",
        "w3",
        "ana",
        "2026-05-11T01:00:00Z",
        "lowercase display name",
    );
    insert_at(
        &storage,
        "whatsapp-family",
        "w4",
        "Bob",
        "2026-05-10T16:46:00Z",
        "other person in the same source",
    );
    // Telegram, same display name, its own source instance.
    let t_apr = insert_at(
        &storage,
        "telegram-work",
        "t1",
        "Ana",
        "2026-04-15T18:00:00Z",
        "telegram one",
    );
    let t_jul = insert_at(
        &storage,
        "telegram-work",
        "t2",
        "Ana",
        "2026-07-20T07:25:00Z",
        "telegram two",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    let whatsapp = instance_for(&storage, "whatsapp-family");
    let telegram = instance_for(&storage, "telegram-work");
    storage
        .link_person_identity(&person.id, &mail_link(&mail, "ana@example.com"))
        .unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&whatsapp, "Ana"))
        .unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&telegram, "Ana"))
        .unwrap();

    // First page: 4 of the 7 matched messages.
    let page = storage
        .person_timeline(&PersonTimelineRequest {
            person_id: person.id.clone(),
            limit: Some(4),
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.person, person);
    assert_eq!(page.limit, 4);

    // Header facts: everything that ever happened, honestly counted.
    assert_eq!(page.header.total_messages, 7);
    assert_eq!(
        page.header.first_contact.as_deref(),
        Some("2026-01-15T12:00:00Z"),
        "first contact is the earliest dated match (the folded mail)"
    );
    assert_eq!(
        page.header.last_contact.as_deref(),
        Some("2026-07-20T07:25:00Z"),
        "last contact is the latest dated match (telegram two)"
    );
    assert_eq!(
        page.header.per_source,
        vec![
            PersonTimelineSourceCount {
                source_instance_id: mail.clone(),
                source_label: "mbox".into(),
                messages: 3,
            },
            PersonTimelineSourceCount {
                source_instance_id: telegram.clone(),
                source_label: "telegram-work".into(),
                messages: 2,
            },
            PersonTimelineSourceCount {
                source_instance_id: whatsapp.clone(),
                source_label: "whatsapp-family".into(),
                messages: 2,
            },
        ],
        "per-source counts cover exactly the matched rows (sorted by label)"
    );

    // Linked identities with their honest anchor evidence: the mail
    // stable key is address-level (confirmed); the display-name
    // anchors are ambiguous (inferred). Each link carries its own
    // match count, preview-style.
    assert_eq!(page.identities.len(), 3);
    let mail_identity = page
        .identities
        .iter()
        .find(|i| i.source_instance_id == mail)
        .unwrap();
    assert_eq!(mail_identity.anchor_kind, PersonAnchorKind::StableKey);
    assert_eq!(mail_identity.anchor_value, "ana@example.com");
    assert_eq!(mail_identity.display_label, "ana@example.com");
    assert_eq!(mail_identity.match_state, PersonLinkMatchState::Confirmed);
    assert_eq!(mail_identity.matched_messages, 3);
    for (instance, label) in [(&telegram, "telegram-work"), (&whatsapp, "whatsapp-family")] {
        let identity = page
            .identities
            .iter()
            .find(|i| &i.source_instance_id == instance)
            .unwrap();
        assert_eq!(identity.anchor_kind, PersonAnchorKind::AuthorName);
        assert_eq!(identity.anchor_value, "Ana");
        assert_eq!(identity.source_label, label);
        assert_eq!(identity.match_state, PersonLinkMatchState::Inferred);
        assert_eq!(identity.matched_messages, 2);
    }

    // The interleaved thread: sent-time order across the three
    // sources, mail first (January), whatsapp (February), mail again
    // (March), telegram (April) — and the never-matches absent.
    assert_eq!(
        page.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![m_jan, w_feb, m_mar, t_apr]
    );
    // Per-row source identity (the badge facts) and channel.
    for (message, label, channel) in [
        (&page.messages[0], "mbox", "email"),
        (&page.messages[1], "whatsapp-family", "email"),
        (&page.messages[2], "mbox", "email"),
        (&page.messages[3], "telegram-work", "email"),
    ] {
        assert_eq!(message.source_label, label);
        assert_eq!(message.channel, channel);
    }
    assert_eq!(page.messages[0].author, "ANA <Ana@Example.COM>");
    assert_eq!(
        page.messages[0].sent_at.as_deref(),
        Some("2026-01-15T12:00:00Z")
    );
    assert!(page.messages[0].snippet.contains("folded author"));

    // The rest of the thread arrives through the keyset cursor alone.
    assert!(page.next_cursor.is_some());
    let drained = drain_through_cursor(&storage, &person.id, 4);
    assert_eq!(
        drained.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![m_jan, w_feb, m_mar, t_apr, w_may, m_jun, t_jul],
        "one interleaved thread, mail + whatsapp + telegram by sent time"
    );

    // A person with no links yet has an empty thread and honest
    // header facts, not an error.
    let loner = storage.create_person("Nobody Linked").unwrap();
    let empty = storage
        .person_timeline(&PersonTimelineRequest {
            person_id: loner.id.clone(),
            limit: None,
            cursor: None,
        })
        .unwrap();
    assert_eq!(empty.header.total_messages, 0);
    assert_eq!(empty.header.first_contact, None);
    assert_eq!(empty.header.last_contact, None);
    assert!(empty.header.per_source.is_empty());
    assert!(empty.messages.is_empty());
    assert!(empty.next_cursor.is_none());
}

/// Unknown instants have no position on the timeline: they page last,
/// deterministically by id, and the keyset cursor pastes correctly
/// across the dated/unknown boundary and the equal-instant tiebreak.
#[test]
fn unknown_instants_sort_last_and_the_cursor_crosses_the_boundary() {
    let storage = Storage::open_in_memory().unwrap();
    let m1 = insert_at(
        &storage,
        "mbox",
        "m1",
        "Ana <ana@example.com>",
        "2026-02-01T10:00:00Z",
        "dated mail",
    );
    // Two messages at the same instant in different sources: the id
    // tiebreak orders them deterministically.
    let t1 = insert_at(
        &storage,
        "telegram-work",
        "t1",
        "Ana",
        "2026-02-01T10:00:00Z",
        "same instant as the mail",
    );
    let t2 = insert_at(
        &storage,
        "telegram-work",
        "t2",
        "Ana",
        "2026-03-01T09:00:00Z",
        "later telegram",
    );
    // Unknown instants (empty sent_at): last, by id.
    let u1 = insert_at(
        &storage,
        "telegram-work",
        "t3",
        "Ana",
        "",
        "no timestamp one",
    );
    let u2 = insert_at(
        &storage,
        "mbox",
        "m2",
        "ana@example.com",
        "",
        "no timestamp two",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    let telegram = instance_for(&storage, "telegram-work");
    storage
        .link_person_identity(&person.id, &mail_link(&mail, "ana@example.com"))
        .unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&telegram, "Ana"))
        .unwrap();

    // Full order: the dated rows by (instant, id), then the unknowns
    // by id. Pages of 2 force the cursor across the tiebreak and the
    // unknown boundary.
    let drained = drain_through_cursor(&storage, &person.id, 2);
    assert_eq!(
        drained.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![m1, t1, t2, u1, u2],
        "equal instants tiebreak by id; unknown instants page last by id"
    );
    for message in &drained[..3] {
        assert!(message.sent_at.is_some());
    }
    for message in &drained[3..] {
        assert_eq!(message.sent_at, None);
        assert_eq!(message.time_confidence, "missing");
    }

    // Header facts come from dated rows only; unknowns still count.
    let page = storage
        .person_timeline(&PersonTimelineRequest {
            person_id: person.id.clone(),
            limit: Some(2),
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.header.total_messages, 5);
    assert_eq!(
        page.header.first_contact.as_deref(),
        Some("2026-02-01T10:00:00Z")
    );
    assert_eq!(
        page.header.last_contact.as_deref(),
        Some("2026-03-01T09:00:00Z")
    );

    // A cursor past the last row is an empty page without a next
    // cursor (the search2 end-of-list contract): this one names a
    // sort position after every row in the fixture — an unknown-
    // instant position with an id above every row, so even the
    // unknowns (which sort last) are before it.
    const PAST_END_CURSOR: &str =
        "eyJ2IjoxLCJtb2RlIjoicGVyc29uX3RpbWVsaW5lIiwic2VudCI6bnVsbCwiaWQiOjk5OTk5OTk5OX0";
    let past_end = storage
        .person_timeline(&PersonTimelineRequest {
            person_id: person.id.clone(),
            limit: Some(2),
            cursor: Some(PAST_END_CURSOR.into()),
        })
        .unwrap();
    assert!(past_end.messages.is_empty());
    assert!(past_end.next_cursor.is_none());
    // Header facts still describe the whole thread on every page.
    assert_eq!(past_end.header.total_messages, 5);
}

/// Refusals stay loud: an unknown person is [`Error::PersonNotFound`],
/// and a malformed or foreign cursor is [`Error::InvalidQuery`] — a
/// bad cursor never silently restarts the thread mid-list.
#[test]
fn unknown_person_and_malformed_cursors_are_refused() {
    let storage = Storage::open_in_memory().unwrap();
    insert_at(
        &storage,
        "mbox",
        "m1",
        "Ana <ana@example.com>",
        "2026-02-01T10:00:00Z",
        "body",
    );
    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    storage
        .link_person_identity(&person.id, &mail_link(&mail, "ana@example.com"))
        .unwrap();

    assert!(matches!(
        storage.person_timeline(&PersonTimelineRequest {
            person_id: "no-such-person".into(),
            limit: None,
            cursor: None,
        }),
        Err(Error::PersonNotFound(_))
    ));

    // `////` decodes as base64 but not as the cursor JSON; the long
    // string is a well-formed cursor of an unknown *version*.
    for bad in [
        "not-cursor-text",
        "////",
        "eyJ2Ijo5OTk5LCJtb2RlIjoicGVyc29uX3RpbWVsaW5lIiwic2VudCI6bnVsbCwiaWQiOjF9",
    ] {
        assert!(
            matches!(
                storage.person_timeline(&PersonTimelineRequest {
                    person_id: person.id.clone(),
                    limit: None,
                    cursor: Some(bad.into()),
                }),
                Err(Error::InvalidQuery(_))
            ),
            "cursor {bad:?} must be refused, never silently ignored"
        );
    }

    // A well-formed cursor of a *foreign* order mode (the search2
    // browse cursor carries mode "browse") is rejected with the mode
    // named, not reinterpreted.
    const FOREIGN_MODE_CURSOR: &str = "eyJ2IjoxLCJtb2RlIjoiYnJvd3NlIiwic2VudCI6bnVsbCwiaWQiOjF9";
    assert!(matches!(
        storage.person_timeline(&PersonTimelineRequest {
            person_id: person.id.clone(),
            limit: None,
            cursor: Some(FOREIGN_MODE_CURSOR.into()),
        }),
        Err(Error::InvalidQuery(_))
    ));
}

/// The candidate listing (the picker's source view) and the person
/// timeline agree on what every linked identity matches: per source,
/// the listing's stable-key and name-only candidates carry the same
/// matched counts, owners, and ambiguity marking the timeline's
/// identity rows report — and unlinked candidates of the same sources
/// stay ownerless.
#[test]
fn candidate_listing_agrees_with_the_timeline_identities_per_source() {
    let storage = Storage::open_in_memory().unwrap();
    // The interleaved fixture, compressed: mail under three author
    // forms of one address, WhatsApp under the verbatim name "Ana"
    // (plus its lowercase spelling and another person), Telegram
    // under the same display name — a different identity.
    insert_at(
        &storage,
        "mbox",
        "m1",
        "ANA <Ana@Example.COM>",
        "2026-01-15T12:00:00Z",
        "mail, folded author",
    );
    insert_at(
        &storage,
        "mbox",
        "m2",
        "Ana <ana@example.com>",
        "2026-03-02T09:00:00Z",
        "mail one",
    );
    insert_at(
        &storage,
        "mbox",
        "m3",
        "ana@example.com",
        "2026-06-01T08:00:00Z",
        "mail two, bare address",
    );
    insert_at(
        &storage,
        "whatsapp-family",
        "w1",
        "Ana",
        "2026-02-20T10:30:00Z",
        "whatsapp one",
    );
    insert_at(
        &storage,
        "whatsapp-family",
        "w2",
        "Ana",
        "2026-05-10T16:45:00Z",
        "whatsapp two",
    );
    insert_at(
        &storage,
        "whatsapp-family",
        "w3",
        "ana",
        "2026-05-11T01:00:00Z",
        "lowercase display name",
    );
    insert_at(
        &storage,
        "telegram-work",
        "t1",
        "Ana",
        "2026-04-15T18:00:00Z",
        "telegram one",
    );

    let person = storage.create_person("Ana Ilic").unwrap();
    let mail = instance_for(&storage, "mbox");
    let whatsapp = instance_for(&storage, "whatsapp-family");
    let telegram = instance_for(&storage, "telegram-work");
    storage
        .link_person_identity(&person.id, &mail_link(&mail, "ana@example.com"))
        .unwrap();
    storage
        .link_person_identity(&person.id, &name_link(&whatsapp, "Ana"))
        .unwrap();
    // Telegram's "Ana" deliberately stays unlinked: the same display
    // name in another source is a different, still-linkable identity.

    let page = storage
        .person_timeline(&PersonTimelineRequest {
            person_id: person.id.clone(),
            limit: None,
            cursor: None,
        })
        .unwrap();

    let listing_of = |instance: &str| {
        storage
            .person_identity_candidates(&PersonIdentityCandidatesRequest {
                source_instance_id: instance.to_string(),
                limit: None,
            })
            .unwrap()
    };

    // Mail: one stable-key candidate folding all three author forms —
    // the same count, owner, and non-ambiguity as the timeline's mail
    // identity row.
    let mail_listing = listing_of(&mail);
    assert_eq!(mail_listing.total, 1);
    let mail_candidate = &mail_listing.candidates[0];
    let mail_identity = page
        .identities
        .iter()
        .find(|i| i.source_instance_id == mail)
        .unwrap();
    assert_eq!(mail_candidate.anchor_kind, mail_identity.anchor_kind);
    assert_eq!(mail_candidate.anchor_value, mail_identity.anchor_value);
    assert_eq!(
        mail_candidate.matched_messages,
        mail_identity.matched_messages
    );
    assert_eq!(mail_candidate.current_owner.as_ref(), Some(&person));
    assert!(!mail_candidate.ambiguous);
    assert_eq!(mail_identity.match_state, PersonLinkMatchState::Confirmed);
    assert_eq!(mail_candidate.matched_messages, 3);

    // WhatsApp: the linked "Ana" candidate agrees with the timeline's
    // identity row, while the verbatim "ana" stays a separate
    // ambiguous, unlinked candidate of the same source.
    let whatsapp_listing = listing_of(&whatsapp);
    assert_eq!(whatsapp_listing.total, 2);
    let ana = whatsapp_listing
        .candidates
        .iter()
        .find(|c| c.anchor_value == "Ana")
        .unwrap();
    let whatsapp_identity = page
        .identities
        .iter()
        .find(|i| i.source_instance_id == whatsapp)
        .unwrap();
    assert_eq!(ana.matched_messages, whatsapp_identity.matched_messages);
    assert_eq!(ana.current_owner.as_ref(), Some(&person));
    assert!(ana.ambiguous);
    assert_eq!(
        whatsapp_identity.match_state,
        PersonLinkMatchState::Inferred
    );
    assert_eq!(ana.matched_messages, 2);
    let lowercase = whatsapp_listing
        .candidates
        .iter()
        .find(|c| c.anchor_value == "ana")
        .unwrap();
    assert_eq!(lowercase.matched_messages, 1);
    assert!(lowercase.current_owner.is_none());
    assert!(lowercase.ambiguous);

    // Telegram: the same display name is a different identity —
    // present as a candidate of its own source, ambiguous, unowned,
    // and absent from the person's timeline identities.
    let telegram_listing = listing_of(&telegram);
    assert_eq!(telegram_listing.total, 1);
    let telegram_candidate = &telegram_listing.candidates[0];
    assert_eq!(telegram_candidate.anchor_kind, PersonAnchorKind::AuthorName);
    assert_eq!(telegram_candidate.matched_messages, 1);
    assert!(telegram_candidate.ambiguous);
    assert!(telegram_candidate.current_owner.is_none());
    assert!(
        page.identities
            .iter()
            .all(|i| i.source_instance_id != telegram),
        "an unlinked identity never rides the person's timeline"
    );
}
