// A7 vault-location engine contracts:
//
// - inspection: write access, free space, filesystem type through an
//   injected mount table (network filesystems refuse the active vault),
//   unrelated-directory refusal, foreign-identity refusal, and
//   newer-schema refusal — an unrelated directory is never prepared
//   for overwrite;
// - preview: a phased plan whose byte estimate covers the database
//   snapshot, every referenced object, and the portable settings when
//   present, with the capacity preflight refusing insufficient space;
//   the move is a verified copy, never an assumed rename;
// - journal: round-trip under the recovery directory in its own file,
//   and a defined recovery action for every phase — origin
//   authoritative before the pointer switch, destination after —
//   always naming exactly one writable vault;
// - a missing external vault is a typed error fact for the recovery
//   screen, never an empty replacement archive.

use std::fs;
use std::path::{Path, PathBuf};

use vault_core::backup::recovery_dir;
use vault_core::media;
use vault_core::object_store::Capacity;
use vault_core::settings::{Settings, SETTINGS_FILE};
use vault_core::storage::{Storage, SUPPORTED_SCHEMA_VERSION};
use vault_core::vault_move::{
    self, detect_filesystem, inspect_location, preview_move, resolve_interrupted_move,
    write_move_journal, LocationDisposition, MoveError, MoveIntent, MoveJournal, MovePhase,
    MovePreviewOptions, MoveRecovery, VaultEndpoint, MOVE_JOURNAL_FILE, MOVE_SLACK_BYTES,
};
use vault_core::vault_session::VaultSession;

/// Real small files committed with the repo (the same fixtures the
/// backup suite uses).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn fixture_bytes(name: &str) -> u64 {
    fs::metadata(fixture(name)).unwrap().len()
}

/// A local-filesystem mount table for inspection (the candidate lives
/// under `/`, on `ext4`).
fn local_mounts() -> String {
    "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw,relatime 0 0\n".to_string()
}

fn nfs_root_mounts() -> String {
    "server:/export / nfs rw,vers=4 0 0\n".to_string()
}

/// One temp workspace per test: a `vault/` data dir plus destinations.
struct Sandbox {
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

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn data_dir(&self) -> PathBuf {
        self.dir("vault")
    }

    /// Seed a real vault: two messages, each carrying one stored
    /// attachment (`photo.jpg`, `beep.wav`), one unlinked registry
    /// object (`plan.pdf`), and a portable settings file. Returns the
    /// exact bytes the estimate should count for objects.
    fn seed_vault(&self) -> u64 {
        let data_dir = self.data_dir();
        let storage = Storage::open(data_dir.join("vault.db")).unwrap();
        let mut budget = Capacity::default();
        for i in 0..2 {
            let (name, kind) = if i == 0 { ("photo.jpg", "image") } else { ("beep.wav", "audio") };
            let (entry, _) = media::store(&data_dir, &fixture(name), name, kind, &mut budget)
                .expect("fixture object stored");
            let mut msg = vault_core::message::NewMessage::new(
                "move-test-source".to_string(),
                format!("move-test-msg-{i}@example.org"),
                format!("Author {i} <a{i}@example.org>"),
                format!("2026-11-1{i}T10:00:00Z"),
                format!("Message body {i}"),
            );
            msg.raw_json = serde_json::json!({ "media": [entry] }).to_string();
            storage
                .commit_import_batch(
                    "move-test-import",
                    "move-test-source",
                    &[msg],
                    &[],
                    &[],
                    0,
                    true,
                )
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
        storage
            .commit_import_batch(
                "move-test-unlinked",
                "move-test-source",
                &[],
                &[],
                &registry,
                0,
                true,
            )
            .unwrap();
        drop(storage);

        let settings = Settings::default();
        settings.save(&data_dir.join(SETTINGS_FILE)).unwrap();

        // The vault gains its identity the way production does: one
        // session open mints it.
        let session = VaultSession::open(&data_dir).unwrap();
        let id = session.vault_id();
        drop(session);
        assert_eq!(
            VaultSession::open(&data_dir).unwrap().vault_id(),
            id,
            "identity is stable"
        );
        fixture_bytes("photo.jpg") + fixture_bytes("beep.wav") + fixture_bytes("plan.pdf")
    }

    /// Run a preview on the vault's own writer connection (the
    /// caller-managed quiesce contract: this test is the single
    /// writer).
    fn preview(
        &self,
        destination: &Path,
        opts: &MovePreviewOptions,
    ) -> Result<vault_move::MovePreview, MoveError> {
        let data_dir = self.data_dir();
        let storage = Storage::open(data_dir.join("vault.db")).unwrap();
        preview_move(
            &data_dir,
            storage.connection(),
            destination,
            &local_mounts(),
            opts,
        )
    }
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// An empty local directory inspects clean: writable, with free space,
/// a local filesystem, and the empty disposition.
#[test]
fn inspection_accepts_an_empty_local_directory() {
    let sb = Sandbox::new("inspect-empty");
    let candidate = sb.dir("candidate");
    let report =
        inspect_location(&candidate, None, &local_mounts()).expect("empty local dir inspects");
    assert_eq!(report.path, candidate);
    assert!(report.writable);
    assert!(report.free_bytes > 0, "free space is reported");
    assert_eq!(report.filesystem.fs_type, "ext4");
    assert!(!report.filesystem.network);
    assert_eq!(report.disposition, LocationDisposition::Empty);
    // Inspection is read-only: nothing but the (removed) probe ran.
    assert_eq!(fs::read_dir(&candidate).unwrap().count(), 0);
}

/// A missing candidate is a typed refusal, never an implicit creation.
#[test]
fn inspection_refuses_a_missing_candidate() {
    let sb = Sandbox::new("inspect-missing");
    let missing = sb.root.join("nope");
    match inspect_location(&missing, None, &local_mounts()) {
        Err(MoveError::Missing { path }) => assert_eq!(path, missing),
        other => panic!("expected a missing refusal, got {other:?}"),
    }
}

/// A network filesystem (through the injected mount table) refuses the
/// candidate: the active WAL vault never lives on one.
#[test]
fn inspection_refuses_a_network_filesystem_via_the_injected_mount_table() {
    let sb = Sandbox::new("inspect-nfs");
    let candidate = sb.dir("candidate");
    match inspect_location(&candidate, None, &nfs_root_mounts()) {
        Err(MoveError::NetworkFilesystem { fs, path }) => {
            assert_eq!(fs, "nfs");
            assert_eq!(path, candidate);
        }
        other => panic!("expected a network-filesystem refusal, got {other:?}"),
    }
    // The longest matching mount wins: a cifs mount over the candidate
    // refuses even under a local root.
    let candidate_real = candidate.canonicalize().unwrap();
    let nested = format!(
        "{}dev/sdb {} cifs rw 0 0\n",
        local_mounts(),
        candidate_real.display()
    );
    match inspect_location(&candidate, None, &nested) {
        Err(MoveError::NetworkFilesystem { fs, .. }) => assert_eq!(fs, "cifs"),
        other => panic!("expected the nested cifs refusal, got {other:?}"),
    }
    // Detection of the same path with the local table is not network.
    assert!(!detect_filesystem(&candidate, &local_mounts()).network);
}

/// A directory with unrelated content and no vault identity is never
/// prepared for overwrite — even content that merely sits next to an
/// identity.
#[test]
fn inspection_refuses_an_unrelated_directory() {
    let sb = Sandbox::new("inspect-unrelated");
    let candidate = sb.dir("candidate");
    fs::write(candidate.join("notes.txt"), "not a vault").unwrap();
    match inspect_location(&candidate, None, &local_mounts()) {
        Err(MoveError::UnrelatedDirectory { path }) => assert_eq!(path, candidate),
        other => panic!("expected an unrelated-directory refusal, got {other:?}"),
    }
    // A stray file next to a *valid* identity still refuses: a vault
    // directory holds vault files only.
    let session = VaultSession::open(&candidate).unwrap();
    drop(session);
    fs::write(candidate.join("extra.bin"), b"x").unwrap();
    match inspect_location(&candidate, None, &local_mounts()) {
        Err(MoveError::UnrelatedDirectory { .. }) => {}
        other => panic!("expected the stray-file refusal, got {other:?}"),
    }
}

/// A directory the process cannot write into refuses with the write
/// probe error. (Skipped when the runner is privileged enough to write
/// through a read-only mode — the probe then legitimately succeeds.)
#[test]
fn inspection_refuses_an_unwritable_directory() {
    let sb = Sandbox::new("inspect-readonly");
    let candidate = sb.dir("candidate");
    let mut perms = fs::metadata(&candidate).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o500);
    fs::set_permissions(&candidate, perms).unwrap();
    let outcome = inspect_location(&candidate, None, &local_mounts());
    let mut perms = fs::metadata(&candidate).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&candidate, perms).unwrap();
    match outcome {
        Err(MoveError::NotWritable { path }) => assert_eq!(path, candidate),
        Ok(report) => {
            // Root (or a privileged LSM) writes through 0o500; the
            // honest check then is that the probe passed and cleaned
            // up after itself.
            assert!(report.writable);
            assert_eq!(fs::read_dir(&candidate).unwrap().count(), 0);
        }
        other => panic!("expected a writability outcome, got {other:?}"),
    }
}

/// A directory holding a valid identity and a supported database
/// reports the existing vault — including its schema version — and the
/// same vault id is accepted as a move destination context.
#[test]
fn inspection_reports_an_existing_compatible_vault() {
    let sb = Sandbox::new("inspect-existing");
    let candidate = sb.dir("candidate");
    // A real migrated database plus a minted identity.
    {
        let storage = Storage::open(candidate.join("vault.db")).unwrap();
        drop(storage);
    }
    let session = VaultSession::open(&candidate).unwrap();
    let id = session.vault_id();
    drop(session);
    let report = inspect_location(&candidate, Some(id), &local_mounts())
        .expect("a compatible vault inspects");
    match report.disposition {
        LocationDisposition::ExistingVault {
            vault_id,
            schema_version,
        } => {
            assert_eq!(vault_id, id);
            assert_eq!(schema_version, Some(SUPPORTED_SCHEMA_VERSION));
        }
        other => panic!("expected an existing vault, got {other:?}"),
    }
    // The same directory under a *foreign* vault context refuses.
    let origin = sb.dir("origin-vault");
    let origin_session = VaultSession::open(&origin).unwrap();
    let origin_id = origin_session.vault_id();
    drop(origin_session);
    match inspect_location(&candidate, Some(origin_id), &local_mounts()) {
        Err(MoveError::IdentityMismatch { existing, active, .. }) => {
            assert_eq!(existing, id.to_string());
            assert_eq!(active, origin_id.to_string());
        }
        other => panic!("expected an identity mismatch, got {other:?}"),
    }
    // Without move context the existing vault is simply reported.
    let bare = inspect_location(&candidate, None, &local_mounts()).unwrap();
    assert!(matches!(
        bare.disposition,
        LocationDisposition::ExistingVault { .. }
    ));
}

/// A vault whose database claims a newer schema refuses exactly like
/// an open would.
#[test]
fn inspection_refuses_a_newer_schema_vault() {
    let sb = Sandbox::new("inspect-schema");
    let candidate = sb.dir("candidate");
    let session = VaultSession::open(&candidate).unwrap();
    drop(session);
    let db = candidate.join("vault.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (version INTEGER NOT NULL);
         INSERT INTO schema_migrations (version) VALUES (999);",
    )
    .unwrap();
    drop(conn);
    match inspect_location(&candidate, None, &local_mounts()) {
        Err(MoveError::SchemaTooNew { db, supported, .. }) => {
            assert_eq!(db, 999);
            assert_eq!(supported, SUPPORTED_SCHEMA_VERSION);
        }
        other => panic!("expected a newer-schema refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// The preview's estimate counts the database snapshot, every
/// referenced object, and the portable settings when present — and the
/// plan is the phased verified copy (never an assumed rename).
#[test]
fn preview_estimates_database_objects_and_settings() {
    let sb = Sandbox::new("preview");
    let object_bytes = sb.seed_vault();
    let settings_bytes = fs::metadata(sb.data_dir().join(SETTINGS_FILE)).unwrap().len();
    let destination = sb.dir("destination");

    let preview = sb
        .preview(&destination, &MovePreviewOptions::default())
        .expect("preview succeeds");

    let origin_id = VaultSession::open(sb.data_dir()).unwrap().vault_id();
    assert_eq!(preview.origin.vault_id, origin_id.to_string());
    assert_eq!(preview.origin.path, sb.data_dir());
    assert_eq!(preview.destination.path, destination);
    // The estimate covers the database (a real migrated snapshot, not
    // a guess), the objects byte-for-byte, and the settings file.
    assert!(
        preview.estimate.database_bytes > 0,
        "database snapshot counted"
    );
    assert_eq!(preview.estimate.object_bytes, object_bytes);
    assert_eq!(preview.estimate.settings_bytes, settings_bytes);
    assert_eq!(preview.estimate.overhead_bytes, MOVE_SLACK_BYTES);
    assert_eq!(
        preview.estimate.total_bytes,
        preview.estimate.database_bytes
            + preview.estimate.object_bytes
            + preview.estimate.settings_bytes
            + preview.estimate.overhead_bytes
    );
    assert!(preview.destination_free_bytes > 0);
    // The plan: all five phases in order, a copy that verifies.
    assert_eq!(preview.phases, MovePhase::PLAN.to_vec());
    assert!(preview.phases[2].summary().contains("checksum verification"));
    assert!(preview.phases[2].summary().contains("never an assumed rename"));
    // Same-device destination detected; the plan still copies.
    assert!(!preview.cross_volume);
    assert!(preview.phases.contains(&MovePhase::Copying));
    // The preview snapshot is cleaned up behind itself.
    let recovery = recovery_dir(&sb.data_dir());
    let leftovers: Vec<_> = fs::read_dir(&recovery)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "preview leaves nothing behind: {leftovers:?}"
    );
}

/// Without a settings file the estimate simply omits it.
#[test]
fn preview_omits_settings_when_absent() {
    let sb = Sandbox::new("preview-no-settings");
    sb.seed_vault();
    fs::remove_file(sb.data_dir().join(SETTINGS_FILE)).unwrap();
    let destination = sb.dir("destination");
    let preview = sb
        .preview(&destination, &MovePreviewOptions::default())
        .unwrap();
    assert_eq!(preview.estimate.settings_bytes, 0);
}

/// The capacity preflight refuses an insufficient destination without
/// writing anything into it.
#[test]
fn preview_refuses_insufficient_space() {
    let sb = Sandbox::new("preview-space");
    sb.seed_vault();
    let destination = sb.dir("destination");
    let opts = MovePreviewOptions {
        min_free_bytes: Some(u64::MAX / 2),
    };
    match sb.preview(&destination, &opts) {
        Err(MoveError::InsufficientSpace { needed, .. }) => assert_eq!(needed, u64::MAX / 2),
        other => panic!("expected the space refusal, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(&destination).unwrap().count(),
        0,
        "the refused preview wrote nothing into the destination"
    );
}

/// A destination the inspection refuses (unrelated content) never
/// reaches the estimate.
#[test]
fn preview_inherits_the_inspection_refusals() {
    let sb = Sandbox::new("preview-refusal");
    sb.seed_vault();
    let destination = sb.dir("destination");
    fs::write(destination.join("existing-work.txt"), "not a vault").unwrap();
    match sb.preview(&destination, &MovePreviewOptions::default()) {
        Err(MoveError::UnrelatedDirectory { .. }) => {}
        other => panic!("expected the inspection refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Journal and resolution
// ---------------------------------------------------------------------------

fn endpoints() -> (VaultEndpoint, VaultEndpoint) {
    (
        VaultEndpoint {
            vault_id: "11111111-1111-4111-8111-111111111111".to_string(),
            path: PathBuf::from("/devices/origin/vault"),
        },
        VaultEndpoint {
            vault_id: "11111111-1111-4111-8111-111111111111".to_string(),
            path: PathBuf::from("/volumes/external/vault"),
        },
    )
}

/// The journal round-trips under the recovery directory, in its own
/// file — never the restore switch journal's.
#[test]
fn move_journal_round_trips_in_its_own_recovery_file() {
    let sb = Sandbox::new("journal");
    let data_dir = sb.data_dir();
    assert_eq!(resolve_interrupted_move(&data_dir).unwrap(), MoveRecovery::None);

    let (origin, destination) = endpoints();
    let mut journal = MoveJournal::new_move(origin.clone(), destination.clone());
    journal.created_at = "2026-11-17T12:00:00Z".to_string();
    write_move_journal(&data_dir, &journal).unwrap();

    let path = vault_move::move_journal_path(&data_dir);
    assert_eq!(path, recovery_dir(&data_dir).join(MOVE_JOURNAL_FILE));
    assert!(path.is_file(), "the journal lives under the recovery dir");
    assert_ne!(
        path.file_name().unwrap().to_string_lossy(),
        "switch-journal.json"
    );
    assert_eq!(vault_move::read_move_journal(&data_dir).unwrap(), Some(journal));
    // The record carries both vault identities and paths plus intent.
    let raw = fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"intent\": \"move\""));
    assert!(raw.contains("11111111-1111-4111-8111-111111111111"));
    assert!(raw.contains("/devices/origin/vault"));
    assert!(raw.contains("/volumes/external/vault"));
}

/// Every defined phase resolves to exactly one defined action — origin
/// authoritative for every pre-pointer phase, destination after the
/// atomic pointer switch — and each outcome names exactly one writable
/// vault: an interrupted move never leaves two.
#[test]
fn every_phase_resolves_to_one_action_with_one_writable_vault() {
    let sb = Sandbox::new("resolve");
    let data_dir = sb.data_dir();
    let (origin, destination) = endpoints();

    let pre_pointer = [
        MovePhase::Quiescing,
        MovePhase::SnapshotReady,
        MovePhase::Copying,
        MovePhase::CopiedVerified,
    ];
    for phase in pre_pointer {
        let mut journal = MoveJournal::new_move(origin.clone(), destination.clone());
        journal.phase = phase;
        write_move_journal(&data_dir, &journal).unwrap();
        let recovery = resolve_interrupted_move(&data_dir)
            .unwrap_or_else(|e| panic!("phase {phase:?} resolves: {e}"));
        match recovery {
            MoveRecovery::KeepOrigin {
                origin: o,
                destination: d,
            } => {
                assert_eq!(o, origin, "{phase:?}: origin authoritative");
                assert_eq!(d, destination);
            }
            other => panic!("{phase:?} keeps the origin, got {other:?}"),
        }
        // Exactly one writable vault, and it is the origin — never the
        // destination, never both.
        assert_eq!(
            resolve_interrupted_move(&data_dir).unwrap().writable_vault(),
            Some(origin.path.as_path()),
            "{phase:?}: the origin is the only writable vault"
        );
    }

    let mut journal = MoveJournal::new_move(origin.clone(), destination.clone());
    journal.phase = MovePhase::SwitchingPointer;
    write_move_journal(&data_dir, &journal).unwrap();
    match resolve_interrupted_move(&data_dir).unwrap() {
        MoveRecovery::AdoptDestination { origin: o, destination: d } => {
            assert_eq!(o, origin);
            assert_eq!(d, destination);
        }
        other => panic!("a pointer-switch crash rolls forward, got {other:?}"),
    }

    let mut journal = MoveJournal::new_move(origin.clone(), destination.clone());
    journal.phase = MovePhase::Completed;
    write_move_journal(&data_dir, &journal).unwrap();
    match resolve_interrupted_move(&data_dir).unwrap() {
        MoveRecovery::Completed { origin: o, destination: d } => {
            assert_eq!(o, origin);
            assert_eq!(d, destination);
        }
        other => panic!("a completed journal reports completion, got {other:?}"),
    }
    // After the pointer moved, the *destination* is the one writable
    // vault — the origin copy is retained, never a second writable.
    assert_eq!(
        resolve_interrupted_move(&data_dir).unwrap().writable_vault(),
        Some(destination.path.as_path())
    );
}

/// Resolution is a pure function of the journal: resolving twice
/// returns the same action (startup may retry resolution freely).
#[test]
fn resolution_is_idempotent() {
    let sb = Sandbox::new("resolve-idempotent");
    let data_dir = sb.data_dir();
    let (origin, destination) = endpoints();
    let mut journal = MoveJournal::new_move(origin, destination);
    journal.phase = MovePhase::Copying;
    write_move_journal(&data_dir, &journal).unwrap();
    let first = resolve_interrupted_move(&data_dir).unwrap();
    let second = resolve_interrupted_move(&data_dir).unwrap();
    assert_eq!(first, second);
}

/// A corrupt journal is an error to surface, never a silent
/// empty-vault restart.
#[test]
fn a_corrupt_journal_is_an_error_not_a_silent_restart() {
    let sb = Sandbox::new("journal-corrupt");
    let data_dir = sb.data_dir();
    let recovery = recovery_dir(&data_dir);
    fs::create_dir_all(&recovery).unwrap();
    fs::write(recovery.join(MOVE_JOURNAL_FILE), "not json").unwrap();
    match resolve_interrupted_move(&data_dir) {
        Err(MoveError::Json(_)) => {}
        other => panic!("expected a corrupt-journal error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The missing-external-vault fact
// ---------------------------------------------------------------------------

/// A missing recorded vault location is the recovery-screen fact: the
/// message says no replacement archive was created.
#[test]
fn a_missing_external_vault_is_a_typed_error_fact() {
    let missing = PathBuf::from("/volumes/usb-stick/vault");
    let err = MoveError::ExternalVaultMissing {
        path: missing.clone(),
    };
    let message = err.to_string();
    assert!(message.contains("no replacement archive was created"));
    assert!(message.contains("/volumes/usb-stick/vault"));
    assert!(matches!(err, MoveError::ExternalVaultMissing { .. }));
}

// ---------------------------------------------------------------------------
// MoveIntent placeholder used by the journal record
// ---------------------------------------------------------------------------

#[test]
fn the_journal_intent_is_recorded_as_a_move() {
    let (origin, destination) = endpoints();
    let journal = MoveJournal::new_move(origin, destination);
    assert_eq!(journal.intent, MoveIntent::Move);
    assert_eq!(journal.version, vault_move::MOVE_JOURNAL_VERSION);
    assert_eq!(journal.phase, MovePhase::Quiescing);
}
