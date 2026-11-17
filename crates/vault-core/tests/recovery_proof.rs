//! A3 recovery proof (plan row A3, section 5.6; evidence row "Safe
//! backup/restore foundation exists"): one repeatable scenario that
//! proves the candidate's full user recovery story end to end, where
//! recovery was previously proven only piecewise.
//!
//! The scenario seeds a populated vault — two source instances, messages
//! with stored media, an unlinked registry object, saved searches,
//! collections with members, confirmed people links, and a settings
//! file — **from copies of the media fixtures in a temporary inputs
//! directory that is deleted before the backup**, proving the vault
//! needs nothing outside itself. It then creates a verified backup and
//! restores it into a fresh data directory through the existing staged
//! path (stage → validate → commit), the cross-machine shape: the
//! per-source credential check reports every source missing.
//!
//! Each guarantee the scenario asserts lives in its own test so the
//! acceptance evidence maps guarantee → test name:
//!
//! - `a_verified_backup_exists_after_the_source_inputs_are_deleted`
//! - `restore_into_a_fresh_directory_preserves_the_archive_and_organization`
//! - `restored_media_objects_verify_by_their_manifest_digests`
//! - `restored_accounts_require_reconnection_and_no_credentials_are_carried`
//! - `the_replaced_fresh_vault_is_kept_as_a_previous_slot_that_still_opens`

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vault_core::backup::{self, RestoreOptions, SwitchState, BACKUP_FORMAT_VERSION};
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::media;
use vault_core::object_store::{Capacity, ObjectStore};
use vault_core::search2::SearchArchiveRequest;
use vault_core::settings::{Settings, SETTINGS_FILE};
use vault_core::storage::{
    Collection, NewPersonIdentityLink, PersonAnchorKind, Person, SavedSearch, SourceInstance,
    Storage, VaultFingerprint,
};

/// Real small files committed with the repo — copied into a temporary
/// inputs directory for seeding, then deleted before the backup.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const MEDIA_FIXTURES: [&str; 3] = ["photo.jpg", "beep.wav", "plan.pdf"];

const MAIL_SOURCE: &str = "proof-mail";
const CHAT_SOURCE: &str = "proof-chat";
const PROFILE_ID: &str = "recovery-proof-profile";

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn no_progress(_: backup::ProgressNote) {}

/// Cross-machine restore shape: no source resolves its credentials on
/// this machine, so every restored account requires reconnection.
fn keyring_has_nothing(_: &SourceInstance) -> bool {
    true
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).unwrap());
    format!("{:x}", hasher.finalize())
}

/// One temp workspace per sandbox: a `vault/` data dir driven through
/// the real pool (the app's shape), plus scratch space for containers
/// and source inputs.
struct Sandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn with_pool<R: Send + 'static>(&self, op: impl FnOnce(&Storage) -> R + Send + 'static) -> R {
        let pool = self.spawn_pool();
        let result = pool
            .submit_write(move |s| -> vault_core::Result<R> { Ok(op(s)) })
            .unwrap();
        pool.shutdown();
        result
    }

    fn spawn_pool(&self) -> Arc<DbPool> {
        fs::create_dir_all(self.data_dir()).unwrap();
        DbPool::spawn(
            self.data_dir().join("vault.db"),
            PoolConfig::default(),
            |_| {},
        )
        .unwrap()
    }

    fn store(&self) -> ObjectStore {
        ObjectStore::new(&self.data_dir())
    }

    fn fingerprint(&self) -> VaultFingerprint {
        self.with_pool(|s| s.vault_fingerprint().unwrap())
    }

    fn backup(&self, name: &str) -> backup::BackupSummary {
        let data_dir = self.data_dir();
        let dest = self.root.join(name);
        let pool = self.spawn_pool();
        let result = pool.submit_write(move |s| {
            backup::create_backup(
                &data_dir,
                s.connection(),
                &dest,
                &no_progress,
                &|| false,
                &backup::BackupOptions::default(),
            )
        });
        pool.shutdown();
        result.unwrap()
    }
}

/// The canonical instance id of a legacy import namespace (the instance
/// exists once its first import batch landed).
fn instance_id(s: &Storage, label: &str) -> String {
    s.list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == "legacy" && i.label == label)
        .unwrap_or_else(|| panic!("no source instance for namespace {label}"))
        .id
}

/// The canonical row id of an archived message, by external id.
fn message_id(s: &Storage, external_id: &str) -> i64 {
    s.connection()
        .query_row(
            "SELECT id FROM archive_messages WHERE external_id = ?1",
            [external_id],
            |r| r.get(0),
        )
        .unwrap()
}

/// Everything the scenario seeded, captured from the original vault
/// before the backup so the restored state can be compared exactly.
struct Seeded {
    messages: u64,
    sources: u64,
    attachment_digests: Vec<String>,
    unlinked_digest: String,
    saved_searches: Vec<SavedSearch>,
    collections: Vec<Collection>,
    collection_members: Vec<(String, Vec<i64>)>,
    people: Vec<Person>,
    person_id: String,
    mail_instance: String,
    chat_instance: String,
}

/// Seed a populated vault through public APIs only: two import
/// namespaces through the write pool, messages with stored media
/// attachments (from copies of the fixtures in `inputs/`), one unlinked
/// registry object, saved searches, collections with cross-source
/// members, a person with confirmed identity links, and a settings
/// file. Returns the deleted inputs directory alongside the snapshot.
fn seed_populated_vault(sb: &Sandbox) -> (PathBuf, Seeded) {
    let data_dir = sb.data_dir();
    fs::create_dir_all(&data_dir).unwrap();

    // Source inputs: copies of the committed fixtures in a temporary
    // inputs directory. Seeding reads these copies; the originals in
    // the repo test tree play no part in the vault.
    let inputs = sb.root.join("inputs");
    fs::create_dir_all(&inputs).unwrap();
    for name in MEDIA_FIXTURES {
        fs::copy(fixture(name), inputs.join(name)).unwrap();
    }

    let mut budget = Capacity::default();
    let mut attachment_digests = Vec::new();
    let pool = sb.spawn_pool();

    // Source 1 (mail namespace): four messages, the first two carrying
    // stored media attachments.
    for i in 0..4usize {
        let import_id = "recovery-proof-mail-import".to_string();
        let author = if i == 0 {
            "Ana <ana@example.com>".to_string()
        } else {
            format!("Author {i} <a{i}@example.org>")
        };
        let mut msg = vault_core::message::NewMessage::new(
            MAIL_SOURCE,
            format!("mail-{i}@example.org"),
            author,
            format!("2026-11-0{}T10:00:00Z", (i % 9) + 1),
            format!("mail message {i} about the renovation invoice"),
        );
        if i < 2 {
            let name = if i == 0 { "photo.jpg" } else { "beep.wav" };
            let (entry, _) = media::store(
                &data_dir,
                &inputs.join(name),
                name,
                if i == 0 { "image" } else { "audio" },
                &mut budget,
            )
            .unwrap();
            attachment_digests.push(entry.sha256.clone());
            msg.raw_json = serde_json::json!({ "media": [entry] }).to_string();
        }
        pool.submit_write(move |s| {
            s.commit_import_batch(&import_id, MAIL_SOURCE, &[msg], &[], &[], 0, true)
                .map(|_| ())
        })
        .unwrap();
    }

    // Source 2 (chat namespace): three plain messages.
    for i in 0..3usize {
        let import_id = "recovery-proof-chat-import".to_string();
        let author = if i == 0 { "Ana".to_string() } else { format!("Chatter {i}") };
        let msg = vault_core::message::NewMessage::new(
            CHAT_SOURCE,
            format!("chat-{i}"),
            author,
            format!("2026-11-0{}T12:00:00Z", (i % 9) + 1),
            format!("chat message {i} about the painter schedule"),
        );
        pool.submit_write(move |s| {
            s.commit_import_batch(&import_id, CHAT_SOURCE, &[msg], &[], &[], 0, true)
                .map(|_| ())
        })
        .unwrap();
    }

    // One unlinked registry object (plan.pdf): stored and registered,
    // referenced by no message.
    let (entry, _) = media::store(
        &data_dir,
        &inputs.join("plan.pdf"),
        "plan.pdf",
        "document",
        &mut budget,
    )
    .unwrap();
    let unlinked_digest = entry.sha256.clone();
    let registry = vec![media::UnlinkedObject {
        sha256: entry.sha256,
        size: entry.size,
        name: "plan.pdf".to_string(),
        kind: "document".to_string(),
    }];
    pool.submit_write(move |s| {
        s.commit_import_batch(
            "recovery-proof-unlinked",
            MAIL_SOURCE,
            &[],
            &[],
            &registry,
            0,
            true,
        )
        .map(|_| ())
    })
    .unwrap();

    // Saved searches: one pinned to the mail instance's canonical id,
    // one plain text filter.
    let mail_instance = pool.with_read(|s| instance_id(s, MAIL_SOURCE));
    let chat_instance = pool.with_read(|s| instance_id(s, CHAT_SOURCE));
    let invoices_request = SearchArchiveRequest {
        cursor: None,
        text: "invoice".into(),
        source_ids: vec![mail_instance.clone()],
        timezone: "Europe/Zagreb".into(),
        ..SearchArchiveRequest::default()
    };
    let request = invoices_request.clone();
    pool.submit_write(move |s| {
        s.upsert_saved_search(None, "renovation invoices", &request)
            .map(|_| ())
    })
    .unwrap();
    let schedule_request = SearchArchiveRequest {
        cursor: None,
        text: "painter".into(),
        ..SearchArchiveRequest::default()
    };
    pool.submit_write(move |s| {
        s.upsert_saved_search(None, "painter schedule", &schedule_request)
            .map(|_| ())
    })
    .unwrap();
    let saved_searches: Vec<SavedSearch> = pool.with_read(|s| s.saved_searches().unwrap());

    // Collections with cross-source members.
    let (mail_msg, chat_msg) =
        pool.with_read(|s| (message_id(s, "mail-0@example.org"), message_id(s, "chat-0")));
    let person_seed = (mail_instance.clone(), chat_instance.clone());
    let person_links_seed = ("ana@example.com", "Ana");
    let (collections, collection_members, person, people) = pool
        .submit_write(move |s| -> vault_core::Result<_> {
            let renovation = s.create_collection("Apartment renovation")?;
            let quotes = s.create_collection("Quotes")?;
            s.add_collection_members(&renovation.id, &[mail_msg, chat_msg])?;
            s.add_collection_members(&quotes.id, &[mail_msg])?;

            let (mail_instance, chat_instance) = &person_seed;
            let (mail_anchor, chat_anchor) = person_links_seed;
            let person = s.create_person("Ana Ilic")?;
            s.link_person_identity(
                &person.id,
                &NewPersonIdentityLink {
                    source_instance_id: mail_instance.as_str(),
                    anchor_kind: PersonAnchorKind::StableKey,
                    anchor_value: mail_anchor,
                    display_label: mail_anchor,
                },
            )?;
            s.link_person_identity(
                &person.id,
                &NewPersonIdentityLink {
                    source_instance_id: chat_instance.as_str(),
                    anchor_kind: PersonAnchorKind::AuthorName,
                    anchor_value: chat_anchor,
                    display_label: chat_anchor,
                },
            )?;
            let collections = s.list_collections()?;
            let members = collections
                .iter()
                .map(|c| {
                    let detail = s.collection_detail(&c.id).unwrap().unwrap();
                    (
                        c.id.clone(),
                        detail.members.iter().map(|m| m.message_id).collect(),
                    )
                })
                .collect();
            let people = s.list_people()?;
            Ok((collections, members, person, people))
        })
        .unwrap();

    let fingerprint = pool.with_read(|s| s.vault_fingerprint().unwrap());
    pool.shutdown();

    // A settings file (provider profile, no key material).
    let mut settings = Settings::default();
    settings.providers.push(vault_core::settings::ProviderProfile {
        id: PROFILE_ID.to_string(),
        name: "Recovery proof".to_string(),
        kind: vault_core::settings::ProviderKind::Custom,
        base_url: "https://example.invalid/api".to_string(),
        model: "proof-model".to_string(),
    });
    settings.save(&data_dir.join(SETTINGS_FILE)).unwrap();

    (
        inputs,
        Seeded {
            messages: fingerprint.messages,
            sources: fingerprint.sources,
            attachment_digests,
            unlinked_digest,
            saved_searches,
            collections,
            collection_members,
            people,
            person_id: person.id,
            mail_instance,
            chat_instance,
        },
    )
}

/// The whole recovery scenario, run once per test: seed the populated
/// vault from copies of the fixtures, delete the source inputs, create
/// a verified backup, restore it into a fresh data directory through
/// the staged path with every source's credentials missing.
struct Scenario {
    #[allow(dead_code)]
    original: Sandbox,
    fresh: Sandbox,
    inputs: PathBuf,
    seed: Seeded,
    summary: backup::BackupSummary,
    outcome: backup::RestoreOutcome,
    /// The fresh vault's message count before the restore (what the
    /// kept previous-vault slot must still hold).
    pre_restore_messages: u64,
}

fn recovered_scenario() -> Scenario {
    let original = Sandbox::new("recovery-proof");
    let (inputs, seed) = seed_populated_vault(&original);

    // The source inputs are gone before anything recovery-related runs:
    // the vault needs nothing outside itself.
    fs::remove_dir_all(&inputs).expect("delete the source inputs directory");
    assert!(!inputs.exists());

    // Verified backup: create, then re-verify by re-hashing every entry.
    let summary = original.backup("recovery-proof.mcbackup");
    backup::verify_backup(&summary.path, &no_progress).expect("the backup re-verifies");

    // Fresh data directory: initialize an empty vault there, then run
    // the one-shot staged restore over it (stage → validate → commit).
    let fresh = Sandbox::new("recovery-proof-fresh");
    fs::create_dir_all(fresh.data_dir()).unwrap();
    {
        let storage = Storage::open(fresh.data_dir().join("vault.db")).unwrap();
        drop(storage);
    }
    let pre_restore = fresh.fingerprint();
    let outcome = backup::restore_backup(
        &fresh.data_dir(),
        &summary.path,
        &pre_restore,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_nothing,
    )
    .expect("staged restore into the fresh directory");

    Scenario {
        original,
        fresh,
        inputs,
        seed,
        summary,
        outcome,
        pre_restore_messages: pre_restore.messages,
    }
}

/// All media-object digests the scenario vault holds (two message
/// attachments plus the unlinked registry object), sorted.
fn all_expected_digests(sc: &Scenario) -> Vec<String> {
    let mut expected = sc.seed.attachment_digests.clone();
    expected.push(sc.seed.unlinked_digest.clone());
    expected.sort();
    expected
}

// ---------------------------------------------------------------------------
// The guarantees
// ---------------------------------------------------------------------------

#[test]
fn a_verified_backup_exists_after_the_source_inputs_are_deleted() {
    let sc = recovered_scenario();
    assert!(
        !sc.inputs.exists(),
        "the source inputs directory is deleted before the backup"
    );

    // The container re-verifies independently (every entry re-hashed
    // against the manifest) and describes the populated vault.
    let verified = backup::verify_backup(&sc.summary.path, &no_progress)
        .expect("the backup re-verifies by re-hashing every entry");
    assert_eq!(verified, sc.summary.manifest);
    assert_eq!(verified.format_version, BACKUP_FORMAT_VERSION);
    assert_eq!(verified.counts.messages, sc.seed.messages);
    assert_eq!(verified.counts.sources, sc.seed.sources);
    assert_eq!(verified.counts.media_objects, 1);
    assert!(verified.settings.is_some(), "settings are carried");

    let mut manifest_digests: Vec<String> = verified
        .objects
        .iter()
        .map(|o| o.sha256.clone())
        .collect();
    manifest_digests.sort();
    assert_eq!(manifest_digests, all_expected_digests(&sc));
}

#[test]
fn restore_into_a_fresh_directory_preserves_the_archive_and_organization() {
    let sc = recovered_scenario();

    // Message counts identical before and after (the fresh vault held
    // nothing; now it holds exactly the original archive).
    let fingerprint = sc.fresh.fingerprint();
    assert_eq!(fingerprint.messages, sc.seed.messages);
    assert_eq!(fingerprint.sources, sc.seed.sources);
    assert_eq!(fingerprint.media_objects, 1);

    // Saved searches survive with their definitions intact.
    let restored_searches: Vec<SavedSearch> =
        sc.fresh.with_pool(|s| s.saved_searches().unwrap());
    assert_eq!(
        restored_searches.len(),
        sc.seed.saved_searches.len(),
        "every saved search survives the restore"
    );
    for expected in &sc.seed.saved_searches {
        let got = restored_searches
            .iter()
            .find(|d| d.name == expected.name)
            .unwrap_or_else(|| panic!("saved search {:?} missing", expected.name));
        assert_eq!(got.request, expected.request);
        assert_eq!(got.id, expected.id);
    }

    // Collections survive with their membership intact.
    let restored_collections: Vec<Collection> =
        sc.fresh.with_pool(|s| s.list_collections().unwrap());
    let restored_names: Vec<String> = restored_collections.iter().map(|c| c.name.clone()).collect();
    let seeded_names: Vec<String> = sc.seed.collections.iter().map(|c| c.name.clone()).collect();
    assert_eq!(restored_names, seeded_names);
    for (seed_collection_id, expected_members) in &sc.seed.collection_members {
        let collection_id = seed_collection_id.clone();
        let detail = sc
            .fresh
            .with_pool(move |s| s.collection_detail(&collection_id).unwrap())
            .expect("collection detail survives");
        let members: Vec<i64> = detail.members.iter().map(|m| m.message_id).collect();
        assert_eq!(&members, expected_members);
        assert!(
            detail.members.iter().all(|m| m.available),
            "every restored member resolves to its archived message"
        );
    }

    // People and their confirmed identity links survive: the person is
    // back, both links resolve, and each anchor finds the person again.
    let restored_people: Vec<Person> = sc.fresh.with_pool(|s| s.list_people().unwrap());
    assert_eq!(restored_people, sc.seed.people);
    let person_id = sc.seed.person_id.clone();
    let links = sc
        .fresh
        .with_pool(move |s| s.person_identity_links(&person_id).unwrap());
    assert_eq!(links.len(), 2, "both confirmed links survive");
    let (mail_instance, chat_instance) =
        (sc.seed.mail_instance.clone(), sc.seed.chat_instance.clone());
    let (mail_person, chat_person) = sc.fresh.with_pool(move |s| {
        (
            s.person_for_anchor(
                &mail_instance,
                PersonAnchorKind::StableKey,
                "ana@example.com",
            )
            .unwrap(),
            s.person_for_anchor(&chat_instance, PersonAnchorKind::AuthorName, "Ana")
                .unwrap(),
        )
    });
    assert!(mail_person.is_some(), "the mail anchor still resolves");
    assert!(chat_person.is_some(), "the chat anchor still resolves");

    // The settings file survives.
    let settings = Settings::load(&sc.fresh.data_dir().join(SETTINGS_FILE))
        .unwrap()
        .expect("settings were restored");
    assert_eq!(settings.providers.len(), 1);
    assert_eq!(settings.providers[0].id, PROFILE_ID);
}

#[test]
fn restored_media_objects_verify_by_their_manifest_digests() {
    let sc = recovered_scenario();

    // Every restored media object re-hashes to its manifest digest.
    let store = sc.fresh.store();
    for object in &sc.summary.manifest.objects {
        let path = store.resolve_object(&object.path).unwrap();
        assert_eq!(
            sha256_hex(&path),
            object.sha256,
            "restored object {} re-hashes to its manifest digest",
            object.path
        );
    }

    // The vault's digest set is exactly the original one (attachments
    // plus the unlinked registry object), and the unlinked object is
    // still registered.
    let digests = sc.fresh.with_pool(|s| s.media_digests().unwrap());
    let mut restored_digests: Vec<String> = digests.into_iter().collect();
    restored_digests.sort();
    assert_eq!(restored_digests, all_expected_digests(&sc));
    let unlinked = sc.fresh.with_pool(|s| s.unlinked_media_objects().unwrap());
    assert_eq!(unlinked.len(), 1);
    assert_eq!(unlinked[0].sha256, sc.seed.unlinked_digest);

    // The restored store reconciles clean against the restored archive.
    let digests = sc.fresh.with_pool(|s| s.media_digests().unwrap());
    let report = store.reconcile(&digests, false).unwrap();
    assert!(
        report.is_clean(),
        "restored store reconciles clean: {report:?}"
    );
}

#[test]
fn restored_accounts_require_reconnection_and_no_credentials_are_carried() {
    let sc = recovered_scenario();

    // The cross-machine shape: the per-source credential check reports
    // every source missing, so the reconnection count equals the source
    // count.
    assert_eq!(sc.seed.sources, 2, "fixture carries two sources");
    assert_eq!(sc.outcome.sources_needing_reauth, sc.seed.sources);
    assert_eq!(sc.outcome.manifest.counts.sources, sc.seed.sources);

    // Credential absence: the container's entry set is exactly the
    // manifest, the database, settings, and the media objects — no
    // keyring export, no extra files.
    let file = fs::File::open(&sc.summary.path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let names: HashSet<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    let mut expected: HashSet<String> = sc
        .summary
        .manifest
        .objects
        .iter()
        .map(|o| o.path.clone())
        .collect();
    expected.insert("manifest.json".to_string());
    expected.insert("vault.db".to_string());
    expected.insert("settings.json".to_string());
    assert_eq!(names, expected);

    // And the settings entry itself carries no key fields.
    let mut entry = zip.by_name("settings.json").unwrap();
    let mut bytes = Vec::new();
    use std::io::Read as _;
    entry.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        !text.to_lowercase().contains("key"),
        "no key material in settings: {text}"
    );
}

#[test]
fn the_replaced_fresh_vault_is_kept_as_a_previous_slot_that_still_opens() {
    let sc = recovered_scenario();

    let status = backup::recovery_status(&sc.fresh.data_dir()).unwrap();
    assert_eq!(status.journal_state, Some(SwitchState::Completed));
    assert_eq!(status.previous.len(), 1, "one recorded slot: {status:?}");
    assert_eq!(status.previous[0].id, sc.outcome.previous_slot);

    // The slot still opens and holds exactly the pre-restore archive
    // (the fresh empty vault the restore replaced).
    let slot = backup::recovery_dir(&sc.fresh.data_dir()).join(&sc.outcome.previous_slot);
    let conn = rusqlite::Connection::open_with_flags(
        slot.join("vault.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count as u64, sc.pre_restore_messages);
}
