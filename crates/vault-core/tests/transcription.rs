// Local voice-note transcription (vault-core backend slice): an
// imported chat voice note becomes searchable by the words spoken in
// it — through the enqueue-only import hook and a separate drain pass
// that runs a stubbed whisper.cpp sidecar (tests never download
// models and never invoke a real whisper binary). Mirrors the spec's
// three acceptance bullets:
//
// 1. with a stub transcriber and conversion tool present (and the
//    setting on), the import itself completes without sidecar work;
//    after the drain pass, plain archive search matches the stubbed
//    spoken words — and did not match before it;
// 2. with the tools absent, an import completes normally, the
//    capability check reports honest unavailability, and no
//    transcripts are produced;
// 3. while the setting is off, nothing is enqueued or transcribed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use vault_core::chat_export::import_bounded;
use vault_core::files;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::transcription::{
    drain_pending, message_transcripts, pending_count, TranscriptionCaps,
};

/// Real small wav fixture (the same `beep.wav` the media suites use),
/// standing in for a preserved voice note's bytes.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// The words the stub transcriber "hears" in every voice note.
const SPOKEN: &str = "the pincode is blue heron";

/// One temp workspace per test: an `export/` tree and a `data/`
/// app-data dir, so store writes never touch the repo (the files.rs
/// sandbox pattern).
struct Sandbox {
    #[allow(dead_code)] // dropped when the sandbox goes out of scope
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

    fn export_dir(&self) -> PathBuf {
        let dir = self.root.join("export");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn data_dir(&self) -> PathBuf {
        let dir = self.root.join("data");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write an executable stub script (`name.sh`) with `body`.
    fn stub(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// A stub whisper binary: prints one whisper.cpp-shaped segment
    /// line on stdout, whatever audio it is handed.
    fn stub_whisper(&self) -> PathBuf {
        self.stub(
            "stub-whisper.sh",
            &format!("#!/bin/sh\nprintf '[00:00:00.000 --> 00:00:02.000] {SPOKEN}\\n'\n"),
        )
    }

    /// A stub ffmpeg: converts by copying the `-i` input to the last
    /// argument (the output wav path) — plumbing-only, so the test
    /// never depends on the runner having real tools.
    fn stub_ffmpeg(&self) -> PathBuf {
        self.stub(
            "stub-ffmpeg.sh",
            "#!/bin/sh
in=''
prev=''
for a in \"$@\"; do
  [ \"$prev\" = \"-i\" ] && in=\"$a\"
  out=\"$a\"
  prev=\"$a\"
done
cp \"$in\" \"$out\"
",
        )
    }

    /// A stub model file (bytes never read; the stem is the model tag).
    fn stub_model(&self) -> PathBuf {
        let path = self.root.join("stub-model.bin");
        fs::write(&path, b"stub model bytes").unwrap();
        path
    }
}

/// A Telegram export tree whose only message is a voice message: the
/// exported `voice/round1.ogg` is the preserved `beep.wav` bytes, and
/// `media_type: voice_message` makes the attachment kind `audio`.
/// Returns the message's archived row id.
fn voice_note_vault(sb: &Sandbox, storage: &mut Storage) -> i64 {
    let voice = sb.export_dir().join("voice");
    fs::create_dir_all(&voice).unwrap();
    fs::copy(
        Path::new(FIXTURES).join("beep.wav"),
        voice.join("round1.ogg"),
    )
    .unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{
  "name": "Arhiva ekipa", "type": "private_group", "id": 123456789,
  "messages": [
    { "id": 201, "type": "message", "date_unixtime": "1698832920",
      "from": "Ana", "file": "voice/round1.ogg",
      "media_type": "voice_message", "file_name": "round1.ogg", "text": "" }
  ]
}"#,
    )
    .unwrap();
    let outcome = import_bounded(
        storage,
        &sb.export_dir(),
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1, "{:?}", outcome.summary);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.stored, 1, "{media:?}");
    let message_id: i64 = storage
        .connection()
        .query_row("SELECT id FROM archive_messages", [], |r| r.get(0))
        .unwrap();
    message_id
}

fn hits(storage: &Storage, term: &str) -> usize {
    storage
        .search_filtered(&build_match_query(term), None, None, None, false, 10)
        .unwrap()
        .len()
}

#[test]
fn imported_voice_note_becomes_searchable_after_the_drain_pass() {
    let sb = Sandbox::new("drain");
    let mut storage = Storage::open(sb.data_dir().join("vault.db")).unwrap();
    let message_id = voice_note_vault(&sb, &mut storage);

    // The import itself enqueued nothing and transcribed nothing:
    // plain search cannot see the spoken words yet.
    assert_eq!(pending_count(&storage).unwrap(), 0);
    assert_eq!(hits(&storage, "pincode"), 0);

    // The reconcile pass with the setting on enqueues exactly one
    // pending row for the preserved audio attachment (and does no
    // sidecar work itself — the drain below is the only sidecar
    // caller).
    let outcome = files::run_backfill_opt(&storage, None, true).unwrap();
    assert!(outcome.completed, "{outcome:?}");
    assert_eq!(pending_count(&storage).unwrap(), 1);

    // Still no match: enqueueing alone must not leak text into search.
    assert_eq!(hits(&storage, "pincode"), 0);

    // Drain through the stub sidecar.
    let caps = TranscriptionCaps::from_paths(sb.stub_whisper(), sb.stub_ffmpeg(), sb.stub_model());
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.attempted, 1, "{drained:?}");
    assert_eq!(drained.transcribed, 1, "{drained:?}");
    assert_eq!(drained.failed, 0, "{drained:?}");

    // Plain search now matches the words that were said — one search
    // over written and spoken words alike.
    assert_eq!(hits(&storage, "pincode"), 1);
    assert_eq!(hits(&storage, "\"blue heron\""), 1);

    // The stored transcript row is the source of truth the reading
    // pane will show (UI slice is follow-up).
    let rows = message_transcripts(&storage, message_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "done");
    assert_eq!(rows[0].text, SPOKEN);
    assert_eq!(rows[0].model, "stub-model");
    assert!(rows[0].transcribed_at.is_some());

    // Re-running the backfill enqueues nothing new (idempotent) and a
    // second drain finds no pending work.
    files::run_backfill_opt(&storage, None, true).unwrap();
    assert_eq!(pending_count(&storage).unwrap(), 0);
    let again = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(again.attempted, 0, "{again:?}");
}

#[test]
fn setting_off_enqueues_and_transcribes_nothing() {
    let sb = Sandbox::new("off");
    let mut storage = Storage::open(sb.data_dir().join("vault.db")).unwrap();
    voice_note_vault(&sb, &mut storage);

    // The default backfill path (transcription off): the projection is
    // built, but no transcript row exists.
    let outcome = files::run_backfill(&storage, None).unwrap();
    assert!(outcome.completed, "{outcome:?}");
    assert!(outcome.refs_upserted >= 1);
    assert_eq!(pending_count(&storage).unwrap(), 0);

    // Even a drain with every tool present finds no work.
    let caps = TranscriptionCaps::from_paths(sb.stub_whisper(), sb.stub_ffmpeg(), sb.stub_model());
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.attempted, 0, "{drained:?}");
    assert_eq!(hits(&storage, "pincode"), 0);
}

#[test]
fn absent_tools_report_honest_unavailability_and_lose_nothing() {
    let sb = Sandbox::new("absent");
    let mut storage = Storage::open(sb.data_dir().join("vault.db")).unwrap();
    voice_note_vault(&sb, &mut storage);

    // Enqueue happens without any sidecar present (it is pure bookkeeping).
    files::run_backfill_opt(&storage, None, true).unwrap();
    assert_eq!(pending_count(&storage).unwrap(), 1);

    // The capability check names exactly what is missing…
    let caps = TranscriptionCaps::default();
    let missing = caps.missing();
    assert_eq!(missing.len(), 3, "{missing:?}");
    let reason = caps.unavailability_reason().expect("honest reason");
    assert!(reason.contains("whisper"), "{reason}");
    assert!(reason.contains("ffmpeg"), "{reason}");
    assert!(reason.contains("model"), "{reason}");

    // …and the drain attempts nothing: the pending row stays for a
    // later pass with the tools in place.
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.attempted, 0, "{drained:?}");
    assert_eq!(drained.transcribed, 0, "{drained:?}");
    assert_eq!(drained.failed, 0, "{drained:?}");
    assert!(drained.unavailable.is_some());
    assert_eq!(pending_count(&storage).unwrap(), 1);
    assert_eq!(hits(&storage, "pincode"), 0);

    // With the tools in place afterwards, the same row drains fine.
    let caps = TranscriptionCaps::from_paths(sb.stub_whisper(), sb.stub_ffmpeg(), sb.stub_model());
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.transcribed, 1, "{drained:?}");
    assert_eq!(hits(&storage, "pincode"), 1);
}

#[test]
fn a_failing_sidecar_skips_the_row_and_retries_later() {
    let sb = Sandbox::new("flaky");
    let mut storage = Storage::open(sb.data_dir().join("vault.db")).unwrap();
    voice_note_vault(&sb, &mut storage);
    files::run_backfill_opt(&storage, None, true).unwrap();

    // A whisper that exits non-zero: skip-and-continue — the pass
    // succeeds, the row stays pending, nothing is indexed.
    let failing = sb.stub("failing-whisper.sh", "#!/bin/sh\nexit 3\n");
    let caps = TranscriptionCaps::from_paths(failing, sb.stub_ffmpeg(), sb.stub_model());
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.attempted, 1, "{drained:?}");
    assert_eq!(drained.transcribed, 0, "{drained:?}");
    assert_eq!(drained.failed, 1, "{drained:?}");
    assert!(drained.last_skip_reason.is_some());
    assert_eq!(pending_count(&storage).unwrap(), 1);
    assert_eq!(hits(&storage, "pincode"), 0);

    // The next drain (sidecar healthy) picks the row up.
    let caps = TranscriptionCaps::from_paths(sb.stub_whisper(), sb.stub_ffmpeg(), sb.stub_model());
    let drained = drain_pending(&storage, &sb.data_dir().join("media"), &caps, 10).unwrap();
    assert_eq!(drained.transcribed, 1, "{drained:?}");
    assert_eq!(hits(&storage, "pincode"), 1);
}

#[test]
fn caps_detection_reports_missing_tools_honestly() {
    // detect() resolves the model from WHISPER_MODEL (an explicit
    // override always wins over the passed default) or from the
    // default path itself. The ambient environment may legitimately
    // carry WHISPER_MODEL (this runner does), so assert the contract
    // branch that actually holds instead of the machine's tools.
    let sb = Sandbox::new("detect");
    let bogus_default = sb.root.join("no-such-model.bin");
    let caps = TranscriptionCaps::detect(&bogus_default);
    let env_model = std::env::var_os("WHISPER_MODEL").map(PathBuf::from);
    match env_model {
        Some(p) if p.is_file() => {
            assert_eq!(caps.model.as_deref(), Some(p.as_path()));
        }
        _ => {
            assert!(caps.model.is_none(), "absent model must not resolve");
            let reason = caps.unavailability_reason();
            assert!(reason.as_deref().unwrap_or_default().contains("model"));
        }
    }

    // An existing model file resolves the model half either way.
    let caps = TranscriptionCaps::detect(&sb.stub_model());
    assert!(caps.model.is_some());
}
