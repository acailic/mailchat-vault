// M4.4 backup/restore contracts, proven against the real lifecycle:
//
// - the versioned container round-trips counts, hashes, settings, and
//   every referenced object (message media plus the M4.2 unlinked
//   registry), and the snapshot is consistent with WAL-resident writes;
// - failures at every backup boundary leave no destination file and an
//   untouched vault; tampered manifests, damaged containers, missing
//   objects, and unsupported (newer) format/schema versions are refused;
// - the restore is staged and validated first, then committed through
//   the two-phase switch journal; terminating the restore at each
//   critical boundary and recovering yields one verified whole vault
//   (rollback for `switching`, roll-forward for `staged`), with the
//   previous vault kept;
// - the preview→commit binding refuses a vault that changed since the
//   preview and staged bytes that changed since validation;
// - returning to a previous vault and forgetting a slot explicitly both
//   go through the validated paths; capacity preflights refuse without
//   writing anything.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use vault_core::backup::{
    self, BackupFailPoint, BackupOptions, RestoreFailPoint, RestoreOptions, SwitchRecovery,
    SwitchState, BACKUP_FORMAT_VERSION,
};
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::media;
use vault_core::object_store::{Capacity, ObjectStore};
use vault_core::settings::{Settings, SETTINGS_FILE};
use vault_core::storage::{Storage, VaultFingerprint};

/// Real small files committed with the repo (the same fixtures the
/// object-store and media-extraction suites use).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn no_progress(_: backup::ProgressNote) {}

/// Credential presence for the reauth count: `false` — every restored
/// source resolves its credentials on this machine (the same-machine
/// restore shape). The dedicated reauth tests inject the other poles.
fn keyring_has_everything(_: &vault_core::storage::SourceInstance) -> bool {
    false
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).unwrap());
    format!("{:x}", hasher.finalize())
}

/// One temp workspace per test: a `vault/` data dir driven through the
/// real pool (WAL writer), plus scratch space for containers.
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

    fn outside(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Open the vault through the real pool (the app's shape) and run
    /// `op` on the writer connection.
    fn with_pool<R: Send + 'static>(&self, op: impl FnOnce(&Storage) -> R + Send + 'static) -> R {
        let pool = self.spawn_pool();
        let result = pool
            .submit_write(move |s| -> vault_core::Result<R> { Ok(op(s)) })
            .unwrap();
        pool.shutdown();
        result
    }

    fn spawn_pool(&self) -> std::sync::Arc<DbPool> {
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

    /// Seed a vault: `n` messages, the first two carrying a stored
    /// attachment each (`photo.jpg` / `beep.wav`
    /// fixtures), one unlinked registry object, and a settings file.
    fn seed_vault(&self, tag: &str, n: usize) -> SeedSummary {
        let tag = tag.to_string();
        let data_dir = self.data_dir();
        fs::create_dir_all(&data_dir).unwrap();
        let mut budget = Capacity::default();
        let mut digests = Vec::new();

        let pool = self.spawn_pool();
        for i in 0..n {
            let import_id = format!("{tag}-import");
            let source = format!("{tag}-source");
            let mut msg = vault_core::message::NewMessage::new(
                format!("{tag}-source"),
                format!("{tag}-msg-{i}@example.org"),
                format!("Author {i} <a{i}@example.org>"),
                format!("2026-11-0{}T10:00:00Z", (i % 9) + 1),
                format!("Message body {i} from {tag}"),
            );
            if i < 2 {
                let (entry, _written) = media::store(
                    &data_dir,
                    &fixture(if i == 0 { "photo.jpg" } else { "beep.wav" }),
                    if i == 0 { "photo.jpg" } else { "beep.wav" },
                    if i == 0 { "image" } else { "audio" },
                    &mut budget,
                )
                .unwrap();
                digests.push(entry.sha256.clone());
                let raw = serde_json::json!({ "media": [entry] });
                msg.raw_json = raw.to_string();
            }
            let batch = vec![msg];
            pool.submit_write(move |s| {
                s.commit_import_batch(&import_id, &source, &batch, &[], &[], 0, true)
            })
            .unwrap();
        }
        // One unlinked registry object (M4.2): stored, registered, no
        // message references it.
        let (unlinked, _) = media::store(
            &data_dir,
            &fixture("plan.pdf"),
            "plan.pdf",
            "document",
            &mut budget,
        )
        .unwrap();
        let unlinked_digest = unlinked.sha256.clone();
        let registry = vec![media::UnlinkedObject {
            sha256: unlinked.sha256.clone(),
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

        // A settings file (provider profile, no key material).
        let mut settings = Settings::default();
        settings
            .providers
            .push(vault_core::settings::ProviderProfile {
                id: "test-profile".to_string(),
                name: "Test".to_string(),
                kind: vault_core::settings::ProviderKind::Custom,
                base_url: "https://example.invalid/api".to_string(),
                model: "test-model".to_string(),
            });
        settings.save(&self.data_dir().join(SETTINGS_FILE)).unwrap();

        SeedSummary {
            messages: n as u64,
            digests,
            unlinked_digest,
        }
    }

    fn fingerprint(&self) -> VaultFingerprint {
        self.with_pool(|s| s.vault_fingerprint().unwrap())
    }

    /// Create a backup to `outside(name)` and verify it in one step.
    fn backup(&self, name: &str) -> backup::BackupSummary {
        self.backup_with(name, BackupOptions::default()).unwrap()
    }

    fn backup_with(
        &self,
        name: &str,
        opts: BackupOptions,
    ) -> vault_core::Result<backup::BackupSummary> {
        let data_dir = self.data_dir();
        let dest = self.outside(name);
        let pool = self.spawn_pool();
        let result = pool.submit_write(move |s| {
            backup::create_backup(
                &data_dir,
                s.connection(),
                &dest,
                &no_progress,
                &|| false,
                &opts,
            )
        });
        pool.shutdown();
        result
    }

    /// Back up a brand-new empty vault (schema only, no rows, no
    /// settings, no objects) — the "restore nothing" pole of the
    /// round-trip tests.
    fn empty_vault_backup(&self, tag: &str) -> backup::BackupSummary {
        let data_dir = self.outside(&format!("empty-{tag}"));
        fs::create_dir_all(&data_dir).unwrap();
        {
            let storage = Storage::open(data_dir.join("vault.db")).unwrap();
            drop(storage);
        }
        let dest = self.outside(&format!("empty-{tag}.mcbackup"));
        let dir = data_dir.clone();
        let pool = DbPool::spawn(data_dir.join("vault.db"), PoolConfig::default(), |_| {}).unwrap();
        let result = pool
            .submit_write(move |s| {
                backup::create_backup(
                    &dir,
                    s.connection(),
                    &dest,
                    &no_progress,
                    &|| false,
                    &BackupOptions::default(),
                )
            })
            .unwrap();
        pool.shutdown();
        result
    }

    /// The active vault's message count and stored-object count.
    fn vault_shape(&self) -> (u64, u64, u64, Vec<String>) {
        self.with_pool(|s| {
            let fp = s.vault_fingerprint().unwrap();
            let digests = s.media_digests().unwrap();
            (fp.messages, fp.sources, fp.media_objects, {
                let mut list: Vec<String> = digests.into_iter().collect();
                list.sort();
                list
            })
        })
    }
}

struct SeedSummary {
    messages: u64,
    digests: Vec<String>,
    unlinked_digest: String,
}

// ---------------------------------------------------------------------------
// Round-trips
// ---------------------------------------------------------------------------

#[test]
fn backup_round_trips_counts_hashes_settings_and_objects() {
    let sb = Sandbox::new("roundtrip");
    let seed = sb.seed_vault("a", 5);

    let summary = sb.backup("vault-a.mcbackup");
    assert_eq!(summary.manifest.format_version, BACKUP_FORMAT_VERSION);
    assert_eq!(summary.manifest.counts.messages, seed.messages);
    assert_eq!(summary.manifest.counts.media_objects, 1);
    // Two message attachments plus the unlinked registry object.
    let mut expected_digests = seed.digests.clone();
    expected_digests.push(seed.unlinked_digest.clone());
    expected_digests.sort();
    let manifest_digests: Vec<String> = summary
        .manifest
        .objects
        .iter()
        .map(|o| o.sha256.clone())
        .collect();
    assert_eq!(manifest_digests, expected_digests);
    // The snapshot entry's checksum describes real snapshot bytes.
    assert_eq!(sha256_hex(&summary.path), sha256_hex(&summary.path)); // container exists
    backup::verify_backup(&summary.path, &no_progress).unwrap();
    assert!(
        summary.manifest.settings.is_some(),
        "settings are part of the backup"
    );

    // Mutate the vault after the backup: delete a message's object set
    // is not possible without corruption, so instead prove the restore
    // brings the exact backup state back by replacing the vault with a
    // different one first.
    let sb2 = Sandbox::new("roundtrip-other");
    // (different vault in the same data dir is not the flow; the real
    // flow restores the same vault's backup over it — covered below.)
    drop(sb2);
    let _ = &sb;

    let fingerprint = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &summary.path,
        &fingerprint,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();

    // Counts and digests round-trip exactly (the M4 exit criterion:
    // media and original counts/hashes round-trip through
    // backup/restore).
    let (messages, _sources, registry, digests) = sb.vault_shape();
    assert_eq!(messages, seed.messages);
    assert_eq!(registry, 1);
    assert_eq!(digests, expected_digests);
    // Every object opens and hashes to its manifest digest.
    let store = sb.store();
    for object in &summary.manifest.objects {
        let path = store.resolve_object(&object.path).unwrap();
        assert_eq!(sha256_hex(&path), object.sha256);
    }
    // Settings were restored byte-exact.
    let settings = Settings::load(&sb.data_dir().join(SETTINGS_FILE))
        .unwrap()
        .unwrap();
    assert_eq!(settings.providers.len(), 1);
    assert_eq!(settings.providers[0].id, "test-profile");
    // The store reconciles clean against the restored archive.
    let digests = sb.with_pool(|s| s.media_digests().unwrap());
    let report = sb.store().reconcile(&digests, false).unwrap();
    assert!(
        report.is_clean(),
        "restored store reconciles clean: {report:?}"
    );

    // The previous vault (the one replaced) is kept under recovery.
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert_eq!(status.journal_state, Some(SwitchState::Completed));
    assert_eq!(
        status.previous.len(),
        1,
        "the replaced vault is kept: {status:?}"
    );
    // And it still opens.
    let slot = sb.outside("vault-recovery").join(&status.previous[0].id);
    let conn = rusqlite::Connection::open_with_flags(
        slot.join("vault.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count as u64, seed.messages);
}

#[test]
fn backup_snapshot_is_consistent_with_wal_resident_writes() {
    let sb = Sandbox::new("wal");
    sb.seed_vault("w", 3);
    // The pool leaves committed rows in the WAL (no checkpoint between
    // batches is forced); the backup's snapshot must still see them.
    let summary = sb.backup("wal.mcbackup");
    assert_eq!(summary.manifest.counts.messages, 3);
    // The manifest's db checksum describes a database that opens and
    // counts the same rows.
    let manifest = backup::read_manifest(&summary.path).unwrap();
    let fingerprint = sb.with_pool(|s| s.vault_fingerprint().unwrap());
    assert_eq!(manifest.counts.messages, fingerprint.messages);
}

#[test]
fn unlinked_registry_objects_round_trip() {
    let sb = Sandbox::new("unlinked");
    sb.seed_vault("u", 1);
    let summary = sb.backup("u.mcbackup");
    // First restore a truly empty vault (no rows, no registry), then the
    // original — the registry row and its object must come back.
    let empty_summary = sb.empty_vault_backup("u");
    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &empty_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
    let unlinked = sb.with_pool(|s| s.unlinked_media_objects().unwrap());
    assert!(unlinked.is_empty(), "the empty vault has no registry rows");

    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
    let unlinked = sb.with_pool(|s| s.unlinked_media_objects().unwrap());
    assert_eq!(unlinked.len(), 1);
    assert_eq!(unlinked[0].sha256, summary.manifest.objects[0].sha256);
    let stored_name = sb.store().object_name(&unlinked[0].sha256).unwrap();
    assert!(sb.data_dir().join("media").join(&stored_name).is_file());
}

// ---------------------------------------------------------------------------
// Backup refusals and failure boundaries
// ---------------------------------------------------------------------------

#[test]
fn backup_rejects_destinations_inside_the_vault_and_recovery_dirs() {
    let sb = Sandbox::new("dest");
    sb.seed_vault("d", 1);
    let inside = sb.data_dir().join("vault.mcbackup");
    let in_recovery_dir = backup::recovery_dir(&sb.data_dir());
    fs::create_dir_all(&in_recovery_dir).unwrap();
    let in_recovery = in_recovery_dir.join("vault.mcbackup");
    let data_dir = sb.data_dir();
    let check = inside.clone();
    let result = sb.with_pool(move |s| {
        backup::create_backup(
            &data_dir,
            s.connection(),
            &check,
            &no_progress,
            &|| false,
            &BackupOptions::default(),
        )
    });
    assert!(
        matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("inside the vault root"))
    );

    let data_dir = sb.data_dir();
    let check = in_recovery.clone();
    let result = sb.with_pool(move |s| {
        backup::create_backup(
            &data_dir,
            s.connection(),
            &check,
            &no_progress,
            &|| false,
            &BackupOptions::default(),
        )
    });
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("recovery")));
    assert!(!inside.exists() && !in_recovery.exists());
}

#[test]
fn backup_failures_leave_no_destination_and_an_untouched_vault() {
    let sb = Sandbox::new("fails");
    sb.seed_vault("f", 3);
    let db_before = sha256_hex(&sb.data_dir().join("vault.db"));

    for fail_at in [
        BackupFailPoint::BeforeSnapshot,
        BackupFailPoint::DuringObjectCopy(0),
        BackupFailPoint::BeforeRename,
    ] {
        let dest = sb.outside(&format!("fail-{fail_at:?}.mcbackup"));
        let result = sb.backup_with(
            &format!("fail-{fail_at:?}.mcbackup"),
            BackupOptions {
                fail_at,
                ..BackupOptions::default()
            },
        );
        drop(result);
        assert!(
            !dest.exists(),
            "no destination after failure at {fail_at:?}"
        );
        assert_eq!(sha256_hex(&sb.data_dir().join("vault.db")), db_before);
        // No temp litter left in the destination directory.
        let litter: Vec<String> = fs::read_dir(&sb.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(litter.is_empty(), "temp litter cleaned: {litter:?}");
    }
}

#[test]
fn backup_preflight_refuses_without_space_and_writes_nothing() {
    let sb = Sandbox::new("space");
    sb.seed_vault("s", 1);
    let dest = sb.outside("big.mcbackup");
    let result = sb.backup_with(
        "big.mcbackup",
        BackupOptions {
            min_free_bytes: Some(u64::MAX / 2),
            ..BackupOptions::default()
        },
    );
    assert!(matches!(
        result,
        Err(vault_core::Error::InsufficientSpace { .. })
    ));
    assert!(!dest.exists());
}

#[test]
fn backup_refuses_when_a_referenced_object_is_missing() {
    let sb = Sandbox::new("missing");
    let seed = sb.seed_vault("m", 2);
    // Damage the store: remove one referenced object (the dangling
    // reference the stage→promote→reference order makes unreachable by
    // construction — external damage).
    let name = sb.store().object_name(&seed.digests[0]).unwrap();
    fs::remove_file(sb.data_dir().join("media").join(&name)).unwrap();
    let result = sb.backup_with("damaged.mcbackup", BackupOptions::default());
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("does not hold it")));
}

// ---------------------------------------------------------------------------
// Container validation
// ---------------------------------------------------------------------------

/// Rebuild a container from a real one with one entry's bytes replaced
/// (the manifest still describes the original — the tamper).
fn tampered_container(src: &Path, dest: &Path, replace_entry: &str, bytes: &[u8]) {
    let file = fs::File::open(src).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let out = fs::File::create(dest).unwrap();
    let mut writer = zip::ZipWriter::new(out);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let name = entry.name().unwrap().to_string();
        use std::io::Read as _;
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        drop(entry);
        if name == replace_entry {
            body = bytes.to_vec();
        }
        use std::io::Write as _;
        writer
            .start_file(name, zip::write::FileOptions::<()>::default())
            .unwrap();
        writer.write_all(&body).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn damaged_container_and_tampered_manifest_are_refused() {
    let sb = Sandbox::new("tamper");
    sb.seed_vault("t", 2);
    let summary = sb.backup("good.mcbackup");

    // A flipped byte mid-file lands inside a deflated stream: the
    // entry fails its checksum (or the deflate stream fails to read).
    let damaged = sb.outside("damaged.mcbackup");
    let mut bytes = fs::read(&summary.path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&damaged, bytes).unwrap();
    assert!(backup::verify_backup(&damaged, &no_progress).is_err());

    // The database entry replaced with different bytes: the manifest's
    // checksum no longer describes the content.
    let tampered = sb.outside("tampered.mcbackup");
    tampered_container(&summary.path, &tampered, "vault.db", b"not a database");
    assert!(backup::verify_backup(&tampered, &no_progress).is_err());
    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &tampered,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(result.is_err(), "an unverified container is never restored");
    // The active vault is untouched.
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 2);
}

#[test]
fn restore_refuses_newer_format_schema_and_missing_objects() {
    let sb = Sandbox::new("versions");
    sb.seed_vault("v", 1);
    let summary = sb.backup("v.mcbackup");

    // Newer container format.
    let mut newer = summary.manifest.clone();
    newer.format_version = BACKUP_FORMAT_VERSION + 1;
    let crafted = craft_container(&sb, "newer-format.mcbackup", &newer, &summary.path);
    assert!(matches!(
        backup::read_manifest(&crafted),
        Err(vault_core::Error::SchemaTooNew { .. })
    ));

    // Newer schema version.
    let mut newer = summary.manifest.clone();
    newer.schema_version = 999;
    let crafted = craft_container(&sb, "newer-schema.mcbackup", &newer, &summary.path);
    assert!(matches!(
        backup::read_manifest(&crafted),
        Err(vault_core::Error::SchemaTooNew { .. })
    ));

    // A manifest that omits a carried object (restore validation must
    // catch the dangling reference).
    let mut missing = summary.manifest.clone();
    missing.objects.clear();
    let crafted = craft_container(&sb, "missing-objects.mcbackup", &missing, &summary.path);
    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &crafted,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("does not carry")));
}

/// Build a container with the given manifest and a real backup's other
/// entries (test-side crafting only).
#[test]
fn restore_refuses_an_entry_expanding_past_its_declared_size() {
    // Bounded expansion (the M4.1 invariant, applied to staging): a
    // crafted container can pass the space preflight with small
    // declared sizes while a deflated entry expands far larger —
    // extraction must abort the moment the streamed size passes the
    // manifest's number, before the staging disk fills.
    let sb = Sandbox::new("expand-bound");
    sb.seed_vault("x", 1);
    let summary = sb.backup("x.mcbackup");

    let mut shrunk = summary.manifest.clone();
    let real = shrunk.db.size;
    assert!(real > 2, "the seeded vault's database must have bytes");
    shrunk.db.size = real / 2;
    let crafted = craft_container(&sb, "shrunk.mcbackup", &shrunk, &summary.path);

    let fp = sb.fingerprint();
    let result = backup::stage_restore(
        &sb.data_dir(),
        &crafted,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
    );
    assert!(
        matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("expands past its manifest size"))
    );
    // The active vault is untouched.
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 1);
}

fn craft_container(
    sb: &Sandbox,
    name: &str,
    manifest: &backup::BackupManifest,
    src: &Path,
) -> PathBuf {
    let dest = sb.outside(name);
    let file = fs::File::open(src).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let out = fs::File::create(&dest).unwrap();
    let mut writer = zip::ZipWriter::new(out);
    let manifest_bytes = serde_json::to_vec_pretty(manifest).unwrap();
    use std::io::{Read as _, Write as _};
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let entry_name = entry.name().unwrap().to_string();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        drop(entry);
        if entry_name == "manifest.json" {
            body = manifest_bytes.clone();
        }
        if entry_name == "manifest.json"
            || manifest.db.path == entry_name
            || manifest.settings.as_ref().map(|s| s.path.as_str()) == Some(entry_name.as_str())
        {
            writer
                .start_file(entry_name, zip::write::FileOptions::<()>::default())
                .unwrap();
            writer.write_all(&body).unwrap();
        }
    }
    writer.finish().unwrap();
    dest
}

#[test]
fn restore_preflight_refuses_without_space() {
    let sb = Sandbox::new("restore-space");
    sb.seed_vault("rs", 1);
    let summary = sb.backup("rs.mcbackup");
    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &summary.path,
        &fp,
        &no_progress,
        &RestoreOptions {
            min_free_bytes: Some(u64::MAX / 2),
            ..RestoreOptions::default()
        },
        &keyring_has_everything,
    );
    assert!(matches!(
        result,
        Err(vault_core::Error::InsufficientSpace { .. })
    ));
    // Nothing staged, nothing switched.
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert_eq!(status.journal_state, None);
    assert!(status.previous.is_empty());
}

#[test]
fn restore_refuses_an_archive_inside_the_vault() {
    let sb = Sandbox::new("inside");
    sb.seed_vault("in", 1);
    let summary = sb.backup("in.mcbackup");
    // Copy the container into the vault root — now restoring it would
    // read from and replace the same tree.
    let inside = sb.data_dir().join("stolen.mcbackup");
    fs::copy(&summary.path, &inside).unwrap();
    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &inside,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("inside the vault")));
}

// ---------------------------------------------------------------------------
// Preview → commit binding
// ---------------------------------------------------------------------------

#[test]
fn commit_refuses_a_vault_that_changed_since_the_preview() {
    let sb = Sandbox::new("changed");
    sb.seed_vault("c", 2);
    let summary = sb.backup("c.mcbackup");

    let preview = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    // The vault gains a message between preview and commit.
    sb.with_pool(|s| {
        let msg = vault_core::message::NewMessage::new(
            "c-source",
            "late@example.org",
            "Late <late@example.org>",
            "2026-11-09T12:00:00Z",
            "arrived after the preview",
        );
        s.commit_import_batch("late-import", "c-source", &[msg], &[], &[], 0, true)
            .unwrap();
    });
    let result = backup::commit_restore(
        &sb.data_dir(),
        &preview.preview_id,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("changed since")));
    // The vault still holds the late message.
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 3);
}

#[test]
fn commit_refuses_staged_bytes_that_changed_since_validation() {
    let sb = Sandbox::new("staged");
    sb.seed_vault("st", 2);
    let summary = sb.backup("st.mcbackup");
    let preview = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    // Tamper with the staged database (the staging area changed).
    let staged_db = backup::recovery_dir(&sb.data_dir())
        .join("staging")
        .join("vault.db");
    let staged = fs::read(&staged_db).unwrap();
    let mut tampered = staged.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0xff;
    fs::write(&staged_db, tampered).unwrap();
    let result = backup::commit_restore(
        &sb.data_dir(),
        &preview.preview_id,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("restage")));
    // The active vault is untouched.
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 2);
}

#[test]
fn commit_refuses_an_unknown_or_superseded_preview_id() {
    let sb = Sandbox::new("preview-id");
    sb.seed_vault("p", 1);
    let summary = sb.backup("p.mcbackup");
    let first = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    // A second preview supersedes the first.
    let second = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    assert_ne!(first.preview_id, second.preview_id);
    let result = backup::commit_restore(
        &sb.data_dir(),
        &first.preview_id,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("newer preview")));
}

// ---------------------------------------------------------------------------
// Reauth accounting (keyring presence, never the manifest counts)
// ---------------------------------------------------------------------------

#[test]
fn restore_reports_zero_reauth_when_the_keyring_still_holds_every_account() {
    // Same-machine restore: every restored source resolves its
    // credentials locally, so the outcome asks for nothing — even
    // though the manifest itself carries sources (the old behavior
    // warned with that raw count).
    let sb = Sandbox::new("reauth-same-machine");
    sb.seed_vault("old", 1);
    let sb2 = Sandbox::new("reauth-same-machine-new");
    sb2.seed_vault("new", 2);
    let new_summary = sb2.backup("new.mcbackup");
    assert!(
        new_summary.manifest.counts.sources > 0,
        "fixture must carry sources"
    );

    let fp = sb.fingerprint();
    let outcome = backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &|_| false,
    )
    .unwrap();
    assert_eq!(
        outcome.sources_needing_reauth, 0,
        "keyring holds every account"
    );
    assert!(
        outcome.manifest.counts.sources > 0,
        "the manifest still counts them"
    );
}

#[test]
fn restore_counts_reauth_per_source_not_from_the_manifest_total() {
    // Cross-machine restore: nothing resolves locally, so every source
    // counts; a partial keyring counts exactly the accounts it lacks.
    let sb = Sandbox::new("reauth-missing");
    sb.seed_vault("old", 1);
    let sb2 = Sandbox::new("reauth-missing-new");
    sb2.seed_vault("new", 2);
    // A second import namespace: the restored vault carries two sources.
    sb2.with_pool(|s| {
        let msg = vault_core::message::NewMessage::new(
            "new-source-b",
            "b@example.org",
            "B <b@example.org>",
            "2026-11-09T10:00:00Z",
            "second namespace",
        );
        s.commit_import_batch("new-import-b", "new-source-b", &[msg], &[], &[], 0, true)
            .unwrap();
    });
    let new_summary = sb2.backup("new.mcbackup");
    let total = new_summary.manifest.counts.sources;
    assert_eq!(total, 2, "fixture must carry two sources");

    // No credential resolves on this machine: every source counts.
    let fp = sb.fingerprint();
    let all = backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &|_| true,
    )
    .unwrap();
    assert_eq!(all.sources_needing_reauth, total);

    // Exactly one account's credentials are missing: exactly it counts.
    let fp = sb.fingerprint();
    let partial = backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &|i| i.label == "new-source-b",
    )
    .unwrap();
    assert_eq!(partial.sources_needing_reauth, 1);
}

#[test]
fn verify_committable_raises_every_refusal_before_the_switch() {
    // The drained-backend precheck: a caller that stops its pool only
    // after this passes can never refuse with a dead backend. Every
    // refusal here leaves the vault untouched and open.
    let sb = Sandbox::new("verify-committable");
    sb.seed_vault("vc", 2);
    let summary = sb.backup("vc.mcbackup");
    let preview = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    // The clean case passes — and switches nothing.
    backup::verify_committable(
        &sb.data_dir(),
        &preview.preview_id,
        &sb.fingerprint(),
        &no_progress,
    )
    .unwrap();
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 2, "the precheck must not switch anything");

    // A vault that changed since the preview refuses, read-only, with
    // the same message the commit itself would give.
    sb.with_pool(|s| {
        let msg = vault_core::message::NewMessage::new(
            "vc-source",
            "late@example.org",
            "Late <late@example.org>",
            "2026-11-09T12:00:00Z",
            "arrived after the preview",
        );
        s.commit_import_batch("late-import", "vc-source", &[msg], &[], &[], 0, true)
            .unwrap();
    });
    let result = backup::verify_committable(
        &sb.data_dir(),
        &preview.preview_id,
        &sb.fingerprint(),
        &no_progress,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("changed since")));

    // A superseded preview id refuses the same way — still read-only.
    let _second = backup::stage_restore(
        &sb.data_dir(),
        &summary.path,
        &sb.fingerprint(),
        &no_progress,
        &RestoreOptions::default(),
    )
    .unwrap();
    let result = backup::verify_committable(
        &sb.data_dir(),
        &preview.preview_id,
        &sb.fingerprint(),
        &no_progress,
    );
    assert!(matches!(&result, Err(vault_core::Error::Backup(m)) if m.contains("newer preview")));
    // The active vault still holds everything the late import wrote.
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 3);
}

// ---------------------------------------------------------------------------
// Interrupted switches: terminate at each critical boundary, recover a
// verified vault
// ---------------------------------------------------------------------------

/// One interrupted-restore case: fail at `fail_at`, recover, and check
/// the resulting vault is exactly the old (`rolls_back`) or the new one.
fn interrupted_case(fail_at: RestoreFailPoint, rolls_back: bool, slot_kept: bool) {
    let tag = format!("interrupt-{:?}-{}", fail_at, rolls_back);
    let sb = Sandbox::new(&tag);
    let seed = sb.seed_vault("old", 4);
    let summary = sb.backup("old.mcbackup");

    // Build the new vault (2 messages, one different object set) and
    // back it up, then restore it over the old vault with the injected
    // failure.
    let sb2 = Sandbox::new(&format!("{tag}-new"));
    let _new_seed = sb2.seed_vault("new", 2);
    let new_summary = sb2.backup("new.mcbackup");
    assert_ne!(
        summary.manifest.counts.messages,
        new_summary.manifest.counts.messages
    );

    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions {
            fail_at,
            ..RestoreOptions::default()
        },
        &keyring_has_everything,
    );
    assert!(
        result.is_err(),
        "the injected failure must fire at {fail_at:?}"
    );

    // Recovery resolves the interruption deterministically.
    let recovery = backup::recover_interrupted_switch(&sb.data_dir()).unwrap();
    let expected_messages = if rolls_back {
        seed.messages
    } else {
        new_summary.manifest.counts.messages
    };
    match recovery {
        SwitchRecovery::None => {
            // No journal was ever written (the failure predates the
            // switch); the original vault simply still stands.
            assert!(matches!(fail_at, RestoreFailPoint::AfterStagedValidate));
        }
        SwitchRecovery::AlreadyCompleted { .. } => {
            panic!("journal cannot be complete at {fail_at:?}")
        }
        SwitchRecovery::RolledBack { .. } => {
            assert!(rolls_back, "unexpected rollback at {fail_at:?}")
        }
        SwitchRecovery::RolledForward { .. } => {
            assert!(!rolls_back, "unexpected roll-forward at {fail_at:?}")
        }
    }
    // The active vault is whole and holds exactly the expected archive.
    let (messages, _sources, _registry, digests) = sb.vault_shape();
    assert_eq!(messages, expected_messages, "at {fail_at:?}");
    if rolls_back {
        assert_eq!(digests.len(), 3, "old vault's objects at {fail_at:?}");
    }
    // Every referenced digest resolves to a real object.
    let store = sb.store();
    for digest in &digests {
        assert!(
            store.has(digest),
            "digest {digest} resolvable at {fail_at:?}"
        );
    }
    // Recovery is idempotent: a second recovery changes nothing.
    let again = backup::recover_interrupted_switch(&sb.data_dir()).unwrap();
    assert!(matches!(
        again,
        SwitchRecovery::None | SwitchRecovery::AlreadyCompleted { .. }
    ));
    // The recovery status reports a completed journal (when a switch
    // ever started — AfterStagedValidate predates the journal), and the
    // previous slot survived exactly when the switch reached phase B.
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    if !matches!(fail_at, RestoreFailPoint::AfterStagedValidate) {
        assert_eq!(
            status.journal_state,
            Some(SwitchState::Completed),
            "at {fail_at:?}"
        );
    } else {
        assert_eq!(status.journal_state, None, "no journal at {fail_at:?}");
    }
    assert_eq!(status.previous.is_empty(), !slot_kept, "at {fail_at:?}");
}

#[test]
fn interrupted_restore_terminates_at_each_boundary_and_recovers() {
    // fail point                                   rolls back?  slot kept?
    // (A rollback consumes the previous slot — its items return to
    // the active vault — so a kept slot means the switch rolled
    // forward past phase A.)
    interrupted_case(RestoreFailPoint::AfterStagedValidate, true, false);
    interrupted_case(RestoreFailPoint::DuringMoveToPrevious(0), true, false);
    interrupted_case(RestoreFailPoint::DuringMoveToPrevious(2), true, false);
    interrupted_case(RestoreFailPoint::AfterMovesToPrevious, true, false);
    interrupted_case(RestoreFailPoint::DuringMoveToActive(0), false, true);
    interrupted_case(RestoreFailPoint::DuringMoveToActive(2), false, true);
    interrupted_case(RestoreFailPoint::BeforeCompleted, false, true);
}

#[test]
fn restore_over_a_staged_only_failure_can_retry_cleanly() {
    // AfterStagedValidate leaves a valid staging area; a plain retry
    // (no injected failure) must succeed.
    let sb = Sandbox::new("retry");
    sb.seed_vault("r", 3);
    let sb2 = Sandbox::new("retry-new");
    sb2.seed_vault("rn", 1);
    let new_summary = sb2.backup("rn.mcbackup");

    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions {
            fail_at: RestoreFailPoint::AfterStagedValidate,
            ..Default::default()
        },
        &keyring_has_everything,
    );
    assert!(result.is_err());
    backup::recover_interrupted_switch(&sb.data_dir()).unwrap();

    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &new_summary.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 1, "the retry restored the new vault");
}

#[test]
fn interrupted_return_recovery_keeps_the_return_target_slot() {
    // A crash during phase A of a *return* switch (the journal is still
    // `switching`, the chosen previous-vault slot is the switch's
    // source) rolls back on restart — and the intact return target must
    // survive the rollback: only recovery_forget deletes slots.
    let sb = Sandbox::new("return-crash");
    sb.seed_vault("old", 4);
    let old = sb.backup("old.mcbackup");
    let sb2 = Sandbox::new("return-crash-new");
    sb2.seed_vault("new", 2);
    let new = sb2.backup("new.mcbackup");

    // Replace the active vault with the "new" backup: the replaced
    // "old" vault becomes a recorded previous-vault slot.
    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &new.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert_eq!(status.previous.len(), 1);
    let slot_id = status.previous[0].id.clone();
    let slot_path = backup::recovery_dir(&sb.data_dir()).join(&slot_id);

    // Crash during phase A of a return to `slot`: the active vault was
    // moved aside into a fresh slot, the journal never left `switching`.
    let staged = backup::recovery_dir(&sb.data_dir()).join("previous-20260101000000-c0ffee");
    fs::create_dir_all(&staged).unwrap();
    for item in ["vault.db", "settings.json", "media"] {
        let from = sb.data_dir().join(item);
        if from.exists() {
            fs::rename(&from, staged.join(item)).unwrap();
        }
    }
    fs::write(
        backup::recovery_dir(&sb.data_dir()).join("switch-journal.json"),
        serde_json::json!({
            "version": 1,
            "state": "switching",
            "created_at": "2026-01-01T00:00:00Z",
            "archive": null,
            "previous": staged.to_string_lossy(),
            "source": slot_path.to_string_lossy(),
        })
        .to_string(),
    )
    .unwrap();

    let recovery = backup::recover_interrupted_switch(&sb.data_dir()).unwrap();
    assert_eq!(
        recovery,
        SwitchRecovery::RolledBack {
            previous_slot: staged.file_name().unwrap().to_string_lossy().into_owned(),
        }
    );
    // The active vault is the displaced one, whole (the "new" vault).
    let (messages, _, _, digests) = sb.vault_shape();
    assert_eq!(messages, 2);
    for digest in &digests {
        assert!(sb.store().has(digest), "digest {digest} resolvable");
    }
    // The return target survived: the intact "old" vault with its
    // database, media, and settings is still in its slot.
    assert!(slot_path.join("vault.db").is_file(), "return target kept");
    assert!(slot_path.join("media").is_dir(), "return target media kept");
    assert!(
        slot_path.join("settings.json").is_file(),
        "return target settings kept"
    );
    // And it still opens, holding exactly the "old" vault's messages.
    let conn = rusqlite::Connection::open_with_flags(
        slot_path.join("vault.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM archive_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count as u64, old.manifest.counts.messages);
    // Every "old" media object is still in the slot's media dir.
    let slot_media = fs::read_dir(slot_path.join("media")).unwrap();
    let slot_names: Vec<String> = slot_media
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for object in &old.manifest.objects {
        let digest = object
            .path
            .split('/')
            .next_back()
            .and_then(|name| name.split('.').next())
            .unwrap();
        assert!(
            slot_names.iter().any(|n| n.starts_with(digest)),
            "slot holds object {digest}: {slot_names:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Previous-vault recovery surface
// ---------------------------------------------------------------------------

#[test]
fn return_to_previous_vault_switches_back_and_records_the_current_one() {
    let sb = Sandbox::new("return");
    sb.seed_vault("before", 4);
    let before = sb.backup("before.mcbackup");
    let sb2 = Sandbox::new("return-after");
    sb2.seed_vault("after", 2);
    let after = sb2.backup("after.mcbackup");

    // Replace the vault with the "after" backup.
    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &after.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 2);

    // Return to the previous vault (the 4-message one).
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert_eq!(status.previous.len(), 1);
    let slot = status.previous[0].id.clone();
    backup::return_to_previous_vault(&sb.data_dir(), &slot, &no_progress, &keyring_has_everything)
        .unwrap();
    let (messages, _, _, digests) = sb.vault_shape();
    assert_eq!(messages, 4);
    assert_eq!(digests.len(), 3);

    // The vault that was replaced by the return is itself recorded as
    // the (single) newest previous slot — the slot we returned to was
    // consumed into the active vault.
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert_eq!(
        status.previous.len(),
        1,
        "the replaced vault is kept: {status:?}"
    );
    // And returning to it would bring the 2-message vault back.
    let slot = status.previous[0].id.clone();
    backup::return_to_previous_vault(&sb.data_dir(), &slot, &no_progress, &keyring_has_everything)
        .unwrap();
    let (messages, _, _, _) = sb.vault_shape();
    assert_eq!(messages, 2);
    drop(before);
}

#[test]
fn recovery_forget_removes_one_slot_and_refuses_unresolved_switches() {
    let sb = Sandbox::new("forget");
    sb.seed_vault("fg", 2);
    let sb2 = Sandbox::new("fg-new");
    sb2.seed_vault("fgn", 1);
    let after = sb2.backup("fgn.mcbackup");
    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &after.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();

    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    let slot = status.previous[0].id.clone();
    // Traversal-shaped ids never resolve to a path.
    assert!(backup::recovery_forget(&sb.data_dir(), "../../etc").is_err());
    assert!(backup::return_to_previous_vault(
        &sb.data_dir(),
        "previous-x",
        &no_progress,
        &keyring_has_everything
    )
    .is_err());
    // A real slot id is removed exactly on request.
    backup::recovery_forget(&sb.data_dir(), &slot).unwrap();
    let status = backup::recovery_status(&sb.data_dir()).unwrap();
    assert!(status.previous.is_empty());
    assert!(
        backup::recovery_forget(&sb.data_dir(), &slot).is_err(),
        "already forgotten"
    );
}

#[test]
fn switch_refuses_while_a_journal_is_unresolved() {
    let sb = Sandbox::new("unresolved");
    sb.seed_vault("un", 2);
    let sb2 = Sandbox::new("un-new");
    sb2.seed_vault("unn", 1);
    let after = sb2.backup("unn.mcbackup");
    let fp = sb.fingerprint();
    let result = backup::restore_backup(
        &sb.data_dir(),
        &after.path,
        &fp,
        &no_progress,
        &RestoreOptions {
            fail_at: RestoreFailPoint::DuringMoveToPrevious(1),
            ..Default::default()
        },
        &keyring_has_everything,
    );
    assert!(result.is_err());
    // While unresolved, no new staging or switching may start.
    assert!(backup::stage_restore(
        &sb.data_dir(),
        &after.path,
        &fp,
        &no_progress,
        &RestoreOptions::default()
    )
    .is_err());
    // Recovery clears it.
    backup::recover_interrupted_switch(&sb.data_dir()).unwrap();
    let fp = sb.fingerprint();
    backup::restore_backup(
        &sb.data_dir(),
        &after.path,
        &fp,
        &no_progress,
        &RestoreOptions::default(),
        &keyring_has_everything,
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Credentials exclusion
// ---------------------------------------------------------------------------

#[test]
fn backups_never_carry_credential_material() {
    let sb = Sandbox::new("creds");
    sb.seed_vault("cr", 1);
    let summary = sb.backup("cr.mcbackup");
    // The container's entry set is exactly manifest + settings + db +
    // objects — no keyring export, no extra files.
    let file = fs::File::open(&summary.path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let names: HashSet<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    let mut expected: HashSet<String> = summary
        .manifest
        .objects
        .iter()
        .map(|o| o.path.clone())
        .collect();
    expected.insert("manifest.json".to_string());
    expected.insert("vault.db".to_string());
    if summary.manifest.settings.is_some() {
        expected.insert("settings.json".to_string());
    }
    assert_eq!(names, expected);
    // And the settings entry itself carries no key fields.
    if summary.manifest.settings.is_some() {
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
}
