//! The A7 vault-location engine: candidate-location inspection, the
//! phased move preview with its estimate, and the crash-recoverable
//! move journal.
//!
//! - inspection covers write access, free space, filesystem type
//!   through the injected mount table (network filesystems refused for
//!   an active WAL vault), unrelated-directory refusal, identity
//!   mismatch, and the newer-schema refusal;
//! - the preview's estimate includes the database, every referenced
//!   object, and the portable settings file when present, with the
//!   plan's phases and the always-verified-copy strategy;
//! - the journal round-trips, and startup resolution turns every
//!   phase into exactly one defined action with the origin
//!   authoritative while no commit executor exists — never a second
//!   writable vault, and a missing post-switch destination surfaces as
//!   the typed unavailable fact with the journal left unresolved.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::media;
use vault_core::object_store::Capacity;
use vault_core::settings::{Settings, SETTINGS_FILE};
use vault_core::storage::SUPPORTED_SCHEMA_VERSION;
use vault_core::vault_move::{
    self, move_plan_phases, new_move_journal, proc_mounts, read_move_journal,
    recover_interrupted_move, vault_location_inspect, vault_move_preview, write_move_journal,
    ExistingContent, FilesystemKind, MoveError, MovePhase, MoveRecovery, MoveStrategy,
    MOVE_JOURNAL_FILE,
};
use vault_core::vault_session::VaultSession;

/// Real small files committed with the repo (the same fixtures the
/// backup and media suites use).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

/// A mount table with `/` as ext4 plus an nfs4 mount at `network` —
/// the injectable source lets tests fixture network mounts without
/// privileges.
fn mounts_fencing(network: &Path) -> String {
    format!(
        "rootfs / rootfs rw 0 0\n\
         /dev/sda1 / ext4 rw,relatime 0 0\n\
         nas:/export/shared {} nfs4 rw,relatime 0 0\n",
        network.display()
    )
}

fn local_mounts() -> String {
    "rootfs / rootfs rw 0 0\n/dev/sda1 / ext4 rw,relatime 0 0\n".to_string()
}

fn mounts_of(text: String) -> impl Fn() -> std::io::Result<String> {
    move || Ok(text.clone())
}

// ---------------------------------------------------------------------------
// Sandbox: a real vault through the real pool (the backup suite's shape)
// ---------------------------------------------------------------------------

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

    fn spawn_pool(&self) -> Arc<DbPool> {
        fs::create_dir_all(self.data_dir()).unwrap();
        DbPool::spawn(
            self.data_dir().join("vault.db"),
            PoolConfig::default(),
            |_| {},
        )
        .unwrap()
    }

    /// Seed the vault: two messages with stored attachments
    /// (`photo.jpg`, `beep.wav`), one unlinked registry object
    /// (`plan.pdf`), and a portable settings file.
    fn seed_vault(&self, tag: &str) -> u64 {
        let data_dir = self.data_dir();
        fs::create_dir_all(&data_dir).unwrap();
        let mut budget = Capacity::default();
        let pool = self.spawn_pool();
        for i in 0..2 {
            let import_id = format!("{tag}-import");
            let source = format!("{tag}-source");
            let name = if i == 0 { "photo.jpg" } else { "beep.wav" };
            let (entry, _written) = media::store(
                &data_dir,
                &fixture(name),
                name,
                if i == 0 { "image" } else { "audio" },
                &mut budget,
            )
            .unwrap();
            let mut msg = vault_core::message::NewMessage::new(
                format!("{tag}-source"),
                format!("{tag}-msg-{i}@example.org"),
                format!("Author {i} <a{i}@example.org>"),
                format!("2026-11-0{}T10:00:00Z", i + 1),
                format!("Message body {i} from {tag}"),
            );
            msg.raw_json = serde_json::json!({ "media": [entry] }).to_string();
            let batch = vec![msg];
            pool.submit_write(move |s| {
                s.commit_import_batch(&import_id, &source, &batch, &[], &[], 0, true)
            })
            .unwrap();
        }
        let (unlinked, _) = media::store(
            &data_dir,
            &fixture("plan.pdf"),
            "plan.pdf",
            "document",
            &mut budget,
        )
        .unwrap();
        let registry = vec![media::UnlinkedObject {
            sha256: unlinked.sha256,
            size: unlinked.size,
            name: "plan.pdf".to_string(),
            kind: "document".to_string(),
        }];
        let registry_import = format!("{tag}-unlinked");
        let registry_source = format!("{tag}-source");
        pool.submit_write(move |s| {
            s.commit_import_batch(
                &registry_import,
                &registry_source,
                &[],
                &[],
                &registry,
                0,
                true,
            )
        })
        .unwrap();
        pool.shutdown();

        let mut settings = Settings::default();
        settings
            .providers
            .push(vault_core::settings::ProviderProfile {
                id: "test-profile".to_string(),
                name: "Test".to_string(),
                kind: vault_core::settings::ProviderKind::Custom,
                base_url: "https://example.invalid/api".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                max_tokens: None,
                timeout_secs: None,
                deployment_name: None,
                api_version: None,
                region: None,
            });
        settings.save(&self.data_dir().join(SETTINGS_FILE)).unwrap();
        3
    }

    /// Open a session so the vault gains its `vault.json` identity,
    /// returning the minted UUID.
    fn ensure_identity(&self) -> String {
        let session = VaultSession::open(self.data_dir()).unwrap();
        let id = session.vault_id().to_string();
        drop(session);
        id
    }

    /// Run the preview through the real pool (the writer-connection
    /// contract the backup snapshot path documents).
    fn preview(&self, destination: &Path) -> Result<vault_move::MovePreview, MoveError> {
        let data_dir = self.data_dir();
        let dest = destination.to_path_buf();
        let pool = self.spawn_pool();
        let result = pool
            .submit_write(move |s| {
                Ok(vault_move_preview(
                    &data_dir,
                    &dest,
                    s.connection(),
                    &proc_mounts,
                ))
            })
            .unwrap();
        pool.shutdown();
        result
    }
}

/// Write an identity file at `dir` (the documented format; only tests
/// and the later commit slice write identities).
fn write_identity(dir: &Path, vault_id: &str) {
    let identity = serde_json::json!({
        "format": "mailchat-vault",
        "format_version": 1,
        "vault_id": vault_id,
        "directory_name": dir.file_name().unwrap().to_string_lossy(),
    });
    fs::write(
        dir.join(vault_core::vault_session::IDENTITY_FILE),
        serde_json::to_string_pretty(&identity).unwrap(),
    )
    .unwrap();
}

/// Create a destination database whose schema version is `version`
/// (the bookkeeping table the supported-version refusal reads).
fn destination_db_at_version(dir: &Path, version: i64) {
    let conn = Connection::open(dir.join(vault_core::vault_session::DB_FILE)).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT \
         NULL DEFAULT (datetime('now')));",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO schema_migrations (version) VALUES (?1)",
        [version],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// A fresh candidate directory inspects as writable, with free space,
/// on a local filesystem, holding nothing.
#[test]
fn inspect_reports_write_access_free_space_and_empty_content() {
    let home = tempfile::tempdir().unwrap();
    let candidate = home.path().join("archive");
    fs::create_dir(&candidate).unwrap();
    let inspection = vault_location_inspect(&candidate, None, &mounts_of(local_mounts())).unwrap();
    assert!(inspection.writable);
    assert!(inspection.free_bytes > 0);
    assert_eq!(
        inspection.filesystem,
        FilesystemKind::Local("ext4".to_string())
    );
    assert_eq!(inspection.existing, ExistingContent::Empty);
    // The probe left nothing behind.
    assert_eq!(fs::read_dir(&candidate).unwrap().count(), 0);
}

/// The production mount table also classifies the runner's own temp
/// filesystem as local (the real /proc/self/mounts, no fixture).
#[test]
fn inspect_on_the_real_mount_table_reports_a_local_filesystem() {
    let candidate = tempfile::tempdir().unwrap();
    let inspection = vault_location_inspect(candidate.path(), None, &proc_mounts).unwrap();
    assert!(matches!(inspection.filesystem, FilesystemKind::Local(_)));
}

/// A candidate on a network filesystem is refused — WAL requires
/// every participant on the same host — through the injected table.
#[test]
fn inspect_refuses_a_network_filesystem_via_the_mount_table() {
    let home = tempfile::tempdir().unwrap();
    let on_nas = home.path().join("nas-archive");
    fs::create_dir(&on_nas).unwrap();
    let canonical = on_nas.canonicalize().unwrap();
    let mounts = mounts_fencing(&canonical);
    match vault_location_inspect(&on_nas, None, &mounts_of(mounts)) {
        Err(MoveError::NetworkFilesystem { fstype, path }) => {
            assert_eq!(fstype, "nfs4");
            assert_eq!(path, canonical);
        }
        other => panic!("expected the network-filesystem refusal, got {other:?}"),
    }
}

/// A missing location, a file where a directory belongs, and a
/// directory holding unrelated content are all refused — an unrelated
/// directory is never prepared for overwrite.
#[test]
fn inspect_refuses_missing_unrelated_and_not_a_directory() {
    let home = tempfile::tempdir().unwrap();
    let mounts = &mounts_of(local_mounts());

    let missing = home.path().join("nope");
    match vault_location_inspect(&missing, None, mounts) {
        Err(MoveError::DirectoryMissing(_)) => {}
        other => panic!("expected a missing-directory refusal, got {other:?}"),
    }

    let a_file = home.path().join("file.txt");
    fs::write(&a_file, b"not a directory").unwrap();
    match vault_location_inspect(&a_file, None, mounts) {
        Err(MoveError::NotADirectory(_)) => {}
        other => panic!("expected a not-a-directory refusal, got {other:?}"),
    }

    let unrelated = home.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("notes.txt"), b"someone's notes").unwrap();
    match vault_location_inspect(&unrelated, None, mounts) {
        Err(MoveError::UnrelatedDirectory(path)) => assert_eq!(path, unrelated),
        other => panic!("expected the unrelated-directory refusal, got {other:?}"),
    }

    // Dot-prefixed leftovers of an aborted move do not make an empty
    // directory look related.
    fs::create_dir(unrelated.parent().unwrap().join("with-staging")).unwrap();
    let with_staging = unrelated.parent().unwrap().join("with-staging");
    fs::create_dir(with_staging.join(vault_move::DESTINATION_STAGING_DIR)).unwrap();
    assert!(vault_location_inspect(&with_staging, None, mounts).is_ok());
}

/// An unwritable candidate is refused (skipped when the test runner
/// itself is privileged enough to ignore the mode).
#[cfg(unix)]
#[test]
fn inspect_refuses_an_unwritable_directory() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    // Privilege canary: a 0000-mode file only stays unwritable when
    // the runner is unprivileged.
    let canary = home.path().join("canary");
    fs::write(&canary, b"").unwrap();
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o000)).unwrap();
    let privileged = fs::OpenOptions::new().write(true).open(&canary).is_ok();
    let _ = fs::remove_file(&canary);
    if privileged {
        return;
    }

    let locked_down = home.path().join("readonly");
    fs::create_dir(&locked_down).unwrap();
    fs::set_permissions(&locked_down, fs::Permissions::from_mode(0o555)).unwrap();
    let result = vault_location_inspect(&locked_down, None, &mounts_of(local_mounts()));
    fs::set_permissions(&locked_down, fs::Permissions::from_mode(0o755)).unwrap();
    match result {
        Err(MoveError::NotWritable(_)) => {}
        other => panic!("expected the not-writable refusal, got {other:?}"),
    }
}

/// A destination holding another vault's identity is refused — a
/// vault is never overwritten — and a malformed identity is refused
/// even with no active vault to compare against.
#[test]
fn inspect_refuses_foreign_and_invalid_identities() {
    let home = tempfile::tempdir().unwrap();
    let mounts = &mounts_of(local_mounts());
    let destination = home.path().join("dest");
    fs::create_dir(&destination).unwrap();

    write_identity(&destination, "11111111-1111-4111-8111-111111111111");
    match vault_location_inspect(
        &destination,
        Some("22222222-2222-4222-8222-222222222222"),
        mounts,
    ) {
        Err(MoveError::DestinationVaultMismatch {
            destination_id,
            active_id,
            ..
        }) => {
            assert_eq!(destination_id, "11111111-1111-4111-8111-111111111111");
            assert_eq!(active_id, "22222222-2222-4222-8222-222222222222");
        }
        other => panic!("expected the identity-mismatch refusal, got {other:?}"),
    }

    // The same vault already sitting at the destination is not a
    // mismatch: it is this vault, relocating onto itself.
    let same = vault_location_inspect(
        &destination,
        Some("11111111-1111-4111-8111-111111111111"),
        mounts,
    )
    .unwrap();
    assert_eq!(
        same.existing,
        ExistingContent::Vault {
            vault_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            schema_version: None,
            directory_name: Some("dest".to_string()),
        }
    );

    // A malformed identity refuses regardless of the active vault.
    let malformed = home.path().join("malformed");
    fs::create_dir(&malformed).unwrap();
    fs::write(
        malformed.join(vault_core::vault_session::IDENTITY_FILE),
        "{\"format\":\"mailchat-vault\",\"format_version\":1,\"vault_id\":\"not-a-uuid\"}",
    )
    .unwrap();
    match vault_location_inspect(&malformed, None, mounts) {
        Err(MoveError::InvalidIdentity { .. }) => {}
        other => panic!("expected the invalid-identity refusal, got {other:?}"),
    }
}

/// A destination database with a newer schema refuses through the
/// existing supported-version refusal; a legacy database (no identity
/// yet) inspects as a vault carrying its schema version.
#[test]
fn inspect_checks_schema_compatibility_of_an_existing_database() {
    let home = tempfile::tempdir().unwrap();
    let mounts = &mounts_of(local_mounts());

    let future = home.path().join("future");
    fs::create_dir(&future).unwrap();
    destination_db_at_version(&future, SUPPORTED_SCHEMA_VERSION + 1);
    match vault_location_inspect(&future, None, mounts) {
        Err(MoveError::SchemaTooNew { db, supported }) => {
            assert_eq!(db, SUPPORTED_SCHEMA_VERSION + 1);
            assert_eq!(supported, SUPPORTED_SCHEMA_VERSION);
        }
        other => panic!("expected the newer-schema refusal, got {other:?}"),
    }

    let legacy = home.path().join("legacy");
    fs::create_dir(&legacy).unwrap();
    destination_db_at_version(&legacy, SUPPORTED_SCHEMA_VERSION);
    let inspection = vault_location_inspect(&legacy, None, mounts).unwrap();
    assert_eq!(
        inspection.existing,
        ExistingContent::Vault {
            vault_id: None,
            schema_version: Some(SUPPORTED_SCHEMA_VERSION),
            directory_name: None,
        }
    );
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// The preview's estimate includes the database snapshot, every
/// referenced object, and the portable settings file; the plan is the
/// five-phase protocol; the copy is always a verified copy; and the
/// preview writes nothing at the destination.
#[test]
fn preview_estimates_database_objects_and_settings() {
    let sb = Sandbox::new("preview");
    let objects = sb.seed_vault("preview");
    sb.ensure_identity();

    let destination = sb.root.join("elsewhere");
    fs::create_dir(&destination).unwrap();
    let preview = sb.preview(&destination).unwrap();

    let expected_objects = ["photo.jpg", "beep.wav", "plan.pdf"]
        .iter()
        .map(|name| fs::metadata(fixture(name)).unwrap().len())
        .sum::<u64>();
    assert_eq!(preview.estimate.object_count, objects);
    assert_eq!(preview.estimate.object_bytes, expected_objects);
    assert!(preview.estimate.db_bytes > 0);
    let settings_len = fs::metadata(sb.data_dir().join(SETTINGS_FILE))
        .unwrap()
        .len();
    assert_eq!(preview.estimate.settings_bytes, Some(settings_len));
    assert_eq!(
        preview.estimate.total_bytes,
        preview.estimate.db_bytes + preview.estimate.object_bytes + settings_len
    );

    let names: Vec<_> = move_plan_phases().iter().map(|p| p.name).collect();
    assert_eq!(names, ["quiesce", "snapshot", "copy", "switch", "restart"]);
    assert_eq!(preview.phases.len(), 5);
    assert_eq!(preview.strategy, MoveStrategy::VerifiedCopy);
    assert!(preview.destination_free_bytes > 0);
    // Same sandbox volume: the plan records that fact but still never
    // assumes a rename.
    assert!(!preview.cross_volume);
    assert_eq!(preview.strategy, MoveStrategy::VerifiedCopy);

    // The preview is read-only for both sides: nothing appeared at the
    // destination, and the vault is untouched.
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert!(sb.data_dir().join("vault.db").is_file());
    // The snapshot temp was discarded, not left in the recovery area.
    let recovery = vault_core::backup::recovery_dir(&sb.data_dir());
    assert!(!fs::read_dir(&recovery)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("snapshot")));
}

/// A preview refuses while an unresolved move journal exists.
#[test]
fn preview_refuses_while_a_move_journal_is_unresolved() {
    let sb = Sandbox::new("preview-journal");
    sb.seed_vault("pj");
    let id = sb.ensure_identity();

    let destination = sb.root.join("elsewhere");
    fs::create_dir(&destination).unwrap();
    let mut journal = new_move_journal(&sb.data_dir(), &destination).unwrap();
    journal.state = MovePhase::Copying;
    write_move_journal(&sb.data_dir(), &journal).unwrap();

    match sb.preview(&destination) {
        Err(MoveError::UnresolvedJournal { state }) => assert_eq!(state, MovePhase::Copying),
        other => panic!("expected the unresolved-journal refusal, got {other:?}"),
    }
    // The journal records both endpoints and the intent.
    let journal = read_move_journal(&sb.data_dir()).unwrap().unwrap();
    assert_eq!(journal.origin.vault_id, id);
    assert_eq!(journal.destination.vault_id, id);
    assert!(journal.intent.contains(&id));
    assert!(journal.intent.contains("move vault"));
}

// ---------------------------------------------------------------------------
// Journal round-trip and resolution
// ---------------------------------------------------------------------------

/// Stage the artifacts an interrupted move would have left: a
/// destination staging area holding a partial copy (with a
/// database-looking file), and an origin-side snapshot staging.
fn stage_interrupted(journal: &vault_move::MoveJournal) {
    fs::create_dir_all(&journal.destination_staging).unwrap();
    fs::write(
        journal.destination_staging.join("vault.db"),
        b"partial copy",
    )
    .unwrap();
    fs::create_dir_all(&journal.origin_staging).unwrap();
    fs::write(
        journal.origin_staging.join("vault.db-snapshot"),
        b"snapshot",
    )
    .unwrap();
}

/// Populate a destination as the finalized vault a pointer switch
/// would have published (identity with the journal's UUID plus a
/// database file).
fn stage_finalized_destination(journal: &vault_move::MoveJournal) {
    write_identity(&journal.destination.path, &journal.destination.vault_id);
    fs::write(
        journal
            .destination
            .path
            .join(vault_core::vault_session::DB_FILE),
        b"finalized destination database",
    )
    .unwrap();
}

fn journal_in_phase(
    origin: &Path,
    destination: &Path,
    phase: MovePhase,
) -> vault_move::MoveJournal {
    let mut journal = new_move_journal(origin, destination).unwrap();
    journal.state = phase;
    write_move_journal(origin, &journal).unwrap();
    journal
}

/// The journal round-trips through disk with its endpoints, staging
/// paths, and intent intact.
#[test]
fn journal_round_trips_through_disk() {
    let sb = Sandbox::new("journal");
    sb.seed_vault("journal");
    let id = sb.ensure_identity();

    let destination = sb.root.join("moved-vault");
    fs::create_dir(&destination).unwrap();
    let journal = new_move_journal(&sb.data_dir(), &destination).unwrap();
    assert_eq!(journal.version, vault_move::MOVE_JOURNAL_VERSION);
    assert_eq!(journal.state, MovePhase::Intended);
    assert_eq!(journal.origin.vault_id, id);
    assert_eq!(journal.destination.vault_id, id);
    assert_eq!(
        journal.destination_staging,
        destination
            .canonicalize()
            .unwrap()
            .join(vault_move::DESTINATION_STAGING_DIR)
    );
    assert!(journal
        .origin_staging
        .ends_with(vault_move::ORIGIN_STAGING_DIR));

    write_move_journal(&sb.data_dir(), &journal).unwrap();
    assert!(vault_move::move_journal_path(&sb.data_dir())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains(MOVE_JOURNAL_FILE));
    let read = read_move_journal(&sb.data_dir()).unwrap().unwrap();
    assert_eq!(read, journal);
}

/// A legacy origin (no identity yet) refuses a new journal: open a
/// session first, which mints the identity.
#[test]
fn new_journal_requires_an_identity() {
    let sb = Sandbox::new("legacy");
    sb.seed_vault("legacy");
    let destination = sb.root.join("elsewhere");
    fs::create_dir(&destination).unwrap();
    match new_move_journal(&sb.data_dir(), &destination) {
        Err(MoveError::Vault(msg)) => assert!(msg.contains("no identity")),
        other => panic!("expected the no-identity refusal, got {other:?}"),
    }
}

/// Startup resolution maps every phase to exactly one action, with the
/// origin authoritative whenever no commit executor exists: staging is
/// discarded, the origin's database is never touched, and no second
/// writable vault survives. The per-phase crash matrix (drop, disk
/// full, permission loss at every boundary) extends this table in the
/// executor ticket.
#[test]
fn recovery_resolves_every_phase_to_one_defined_action() {
    let interrupted_phases = [
        MovePhase::Intended,
        MovePhase::Quiesced,
        MovePhase::Snapshotted,
        MovePhase::Copying,
        MovePhase::Copied,
    ];
    for phase in interrupted_phases {
        let sb = Sandbox::new("recover");
        sb.seed_vault("recover");
        sb.ensure_identity();
        let origin_db = sb.data_dir().join("vault.db");
        let origin_bytes = fs::read(&origin_db).unwrap();

        let destination = sb.root.join("moved-vault");
        fs::create_dir(&destination).unwrap();
        let journal = journal_in_phase(&sb.data_dir(), &destination, phase);
        stage_interrupted(&journal);

        let recovery = recover_interrupted_move(&sb.data_dir()).unwrap();
        match (phase, recovery) {
            (MovePhase::Intended, MoveRecovery::Cancelled { journal: resolved }) => {
                assert_eq!(resolved.state, MovePhase::Intended);
            }
            (
                MovePhase::Quiesced | MovePhase::Snapshotted | MovePhase::Copying,
                MoveRecovery::ResumedOrigin { journal, discarded },
            ) => {
                assert_eq!(journal.state, phase);
                assert_eq!(discarded.len(), 2, "both staging areas were discarded");
                assert!(discarded.contains(&journal.origin_staging));
                assert!(discarded.contains(&journal.destination_staging));
            }
            (
                MovePhase::Copied,
                MoveRecovery::RolledBackToOrigin {
                    journal,
                    discarded_copy,
                },
            ) => {
                assert_eq!(journal.state, phase);
                assert_eq!(discarded_copy, journal.destination_staging);
                assert!(!discarded_copy.exists(), "the duplicate copy is gone");
            }
            (phase, other) => panic!("phase {phase:?} resolved to {other:?}"),
        }

        // Every interrupted phase leaves the journal completed, the
        // origin intact and authoritative, and no vault at the
        // destination — never a second writable vault.
        assert_eq!(
            read_move_journal(&sb.data_dir()).unwrap().unwrap().state,
            MovePhase::Completed
        );
        assert_eq!(fs::read(&origin_db).unwrap(), origin_bytes);
        assert!(!destination
            .join(vault_core::vault_session::DB_FILE)
            .exists());
        assert!(VaultSession::open(sb.data_dir()).is_ok());

        // A resolved journal needs no further action.
        assert!(matches!(
            recover_interrupted_move(&sb.data_dir()).unwrap(),
            MoveRecovery::AlreadyCompleted { .. }
        ));
    }
}

/// A journal past the pointer switch rolls forward: the destination is
/// the vault to open, the origin copy is retained until separately
/// removed.
#[test]
fn recovery_activates_the_destination_after_the_pointer_switch() {
    let sb = Sandbox::new("switched");
    sb.seed_vault("switched");
    sb.ensure_identity();
    let destination = sb.root.join("moved-vault");
    fs::create_dir(&destination).unwrap();
    let journal = journal_in_phase(&sb.data_dir(), &destination, MovePhase::PointerSwitched);
    stage_finalized_destination(&journal);
    // Leftover staging from the finished copy is cleaned up.
    stage_interrupted(&journal);

    let recovery = recover_interrupted_move(&sb.data_dir()).unwrap();
    match recovery {
        MoveRecovery::ActivatedDestination { journal } => {
            assert_eq!(
                journal.destination.path,
                destination.canonicalize().unwrap()
            );
        }
        other => panic!("expected destination activation, got {other:?}"),
    }
    assert_eq!(
        read_move_journal(&sb.data_dir()).unwrap().unwrap().state,
        MovePhase::Completed
    );
    // The destination stayed the vault it was published as, and the
    // origin copy is retained (never auto-deleted).
    assert_eq!(
        fs::read(destination.join(vault_core::vault_session::DB_FILE)).unwrap(),
        b"finalized destination database"
    );
    assert!(sb.data_dir().join("vault.db").is_file());
}

/// A pointer-switched journal whose destination is gone resolves to
/// the typed unavailable fact with the journal left unresolved — the
/// recovery screen path; no empty replacement archive is created.
#[test]
fn recovery_reports_a_missing_destination_as_a_typed_fact() {
    let sb = Sandbox::new("unavailable");
    sb.seed_vault("unavailable");
    sb.ensure_identity();
    let destination = sb.root.join("detached-drive");
    fs::create_dir_all(&destination).unwrap();
    let journal = journal_in_phase(&sb.data_dir(), &destination, MovePhase::PointerSwitched);
    // The external drive disappeared after the journal was written.
    fs::remove_dir_all(&destination).unwrap();

    match recover_interrupted_move(&sb.data_dir()) {
        Err(MoveError::VaultUnavailable(path)) => assert_eq!(path, journal.destination.path),
        other => panic!("expected the vault-unavailable fact, got {other:?}"),
    }
    // Unresolved: reattaching the location lets the next startup
    // finish the activation, and the origin is untouched meanwhile.
    assert_eq!(
        read_move_journal(&sb.data_dir()).unwrap().unwrap().state,
        MovePhase::PointerSwitched
    );
    assert!(sb.data_dir().join("vault.db").is_file());

    // Reattach and resolve: the defined action is still activation.
    fs::create_dir(&destination).unwrap();
    stage_finalized_destination(&journal);
    assert!(matches!(
        recover_interrupted_move(&sb.data_dir()).unwrap(),
        MoveRecovery::ActivatedDestination { .. }
    ));
}

/// A destination that exists but holds a different vault's identity
/// after the pointer switch is a refusal, never a silent switch.
#[test]
fn recovery_refuses_a_switched_destination_with_the_wrong_identity() {
    let sb = Sandbox::new("wrong-id");
    sb.seed_vault("wrong-id");
    sb.ensure_identity();
    let destination = sb.root.join("other-vault");
    fs::create_dir(&destination).unwrap();
    journal_in_phase(&sb.data_dir(), &destination, MovePhase::PointerSwitched);
    write_identity(&destination, "99999999-9999-4999-8999-999999999999");
    fs::write(
        destination.join(vault_core::vault_session::DB_FILE),
        b"someone else's vault",
    )
    .unwrap();

    match recover_interrupted_move(&sb.data_dir()) {
        Err(MoveError::InvalidIdentity { .. }) => {}
        other => panic!("expected the wrong-identity refusal, got {other:?}"),
    }
    assert_eq!(
        read_move_journal(&sb.data_dir()).unwrap().unwrap().state,
        MovePhase::PointerSwitched
    );
}

/// With no journal at all, resolution is a no-op.
#[test]
fn recovery_without_a_journal_does_nothing() {
    let sb = Sandbox::new("none");
    sb.seed_vault("none");
    assert_eq!(
        recover_interrupted_move(&sb.data_dir()).unwrap(),
        MoveRecovery::None
    );
}
