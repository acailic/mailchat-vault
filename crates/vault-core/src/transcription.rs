//! Local voice-note transcription (optional whisper.cpp sidecar).
//!
//! Spoken words in voice notes are invisible to archive search: the
//! media pipeline ([`crate::media`]) preserves the audio bytes and the
//! attachment projection ([`crate::files`], `attachment_refs`) records
//! the digest, but nothing reads them. This module is the backend core
//! that makes them searchable, fully offline:
//!
//! 1. **Enqueue-only at import/reconcile time.** The attachment
//!    backfill — the single writer of `attachment_refs`, so the one
//!    path every import and reconcile funnels through — records
//!    `pending` rows in the `transcripts` table (migration 28) for
//!    audio-kind references whose bytes are preserved. No sidecar is
//!    invoked, no audio converted: imports stay exactly as fast with
//!    transcription on, and a missing or failing sidecar can never
//!    fail an import.
//! 2. **A separate drain pass.** [`drain_pending`] processes pending
//!    rows outside the import path, one row per transaction, through
//!    the optional sidecar: ffmpeg (when present) converts the
//!    preserved bytes to 16 kHz mono WAV, then the whisper.cpp binary
//!    transcribes them. Any per-row failure is skip-and-continue — the
//!    row stays `pending` and the next drain retries it — mirroring
//!    the honest-unavailability pattern the files layer uses.
//! 3. **One search index.** A done transcript lands in the
//!    digest-keyed `transcripts` row (message id, sha256, language,
//!    text, model, transcribed-at) and its text is appended to the
//!    trigger-maintained `archive_fts` document via the
//!    `archive_messages.transcript` column, so plain search matches
//!    written and spoken words alike.
//!
//! Everything is strictly opt-in ([`crate::settings::Options`]
//! `transcribe_audio`, default off): while the setting is off nothing
//! is enqueued and nothing transcribed.
//!
//! # Sidecar resolution
//!
//! - **whisper binary**: the `WHISPER_BIN` environment variable wins;
//!   otherwise `whisper-cli` is looked up on `PATH`. A binary bundled
//!   with the app is pointed at with `WHISPER_BIN` (see the README's
//!   transcription section).
//! - **model**: the `WHISPER_MODEL` environment variable, else the
//!   documented default `<data_dir>/whisper/ggml-base.bin` passed by
//!   the caller. Tests never download models and never invoke a real
//!   binary — the plumbing is proven with stub scripts.
//!
//! Real whisper.cpp output parsing ([`parse_whisper_text`]) follows
//! the segment lines the CLI prints; it is exercised against real
//! binaries only in manual verification (the runner has no model).

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection, Transaction};

use crate::storage::Storage;
use crate::Result;

/// The whisper.cpp binary name looked up on `PATH` when `WHISPER_BIN`
/// is not set (the server-style `whisper-cli` name of current
/// whisper.cpp releases).
pub const WHISPER_BIN_NAME: &str = "whisper-cli";

/// Environment variable holding an explicit whisper binary path.
pub const WHISPER_BIN_ENV: &str = "WHISPER_BIN";

/// Environment variable holding an explicit model path.
pub const WHISPER_MODEL_ENV: &str = "WHISPER_MODEL";

/// One resolved audio attachment queued for transcription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTranscript {
    pub id: i64,
    pub message_id: i64,
    pub sha256: String,
}

/// A stored transcript row (migration 28), as the reading pane and the
/// voice badge will read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub message_id: i64,
    pub sha256: String,
    /// `'pending'` (enqueued, no text yet) or `'done'`.
    pub status: String,
    pub language: String,
    pub text: String,
    pub model: String,
    /// RFC 3339-ish SQLite `datetime('now')` stamp; `None` while
    /// pending.
    pub transcribed_at: Option<String>,
}

/// The sidecar tools transcription needs, resolved once per pass.
///
/// Every field is `None` when the tool is absent: [`Self::missing`]
/// names what is absent and [`drain_pending`] reports that honestly
/// instead of touching the pending rows.
#[derive(Debug, Clone, Default)]
pub struct TranscriptionCaps {
    /// The whisper.cpp binary (`WHISPER_BIN`, or `whisper-cli` on
    /// `PATH`).
    pub whisper_bin: Option<PathBuf>,
    /// The ffmpeg binary used for 16 kHz mono WAV conversion.
    pub ffmpeg_bin: Option<PathBuf>,
    /// The whisper model file (GGML).
    pub model: Option<PathBuf>,
}

impl TranscriptionCaps {
    /// Resolve the sidecar from the environment: `WHISPER_BIN` (else
    /// `whisper-cli` on `PATH`), `ffmpeg` on `PATH`, and the model at
    /// `WHISPER_MODEL` (else `default_model` — the app passes its
    /// data-dir default).
    pub fn detect(default_model: &Path) -> Self {
        let whisper_bin = std::env::var_os(WHISPER_BIN_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| find_on_path(WHISPER_BIN_NAME));
        let model = std::env::var_os(WHISPER_MODEL_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| {
                let default = default_model.to_path_buf();
                default.is_file().then_some(default)
            });
        Self {
            whisper_bin,
            ffmpeg_bin: find_on_path("ffmpeg"),
            model,
        }
    }

    /// Caps built from explicit paths (tests, stub sidecars).
    pub fn from_paths(whisper_bin: PathBuf, ffmpeg_bin: PathBuf, model: PathBuf) -> Self {
        Self {
            whisper_bin: Some(whisper_bin),
            ffmpeg_bin: Some(ffmpeg_bin),
            model: Some(model),
        }
    }

    /// What is absent, in setup-instruction order.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.whisper_bin.is_none() {
            missing.push("whisper binary (set WHISPER_BIN or install whisper-cli)");
        }
        if self.ffmpeg_bin.is_none() {
            missing.push("ffmpeg");
        }
        if self.model.is_none() {
            missing.push("whisper model file (set WHISPER_MODEL)");
        }
        missing
    }

    /// Honest unavailability: `None` when every tool is present, else
    /// one sentence naming exactly what is missing.
    pub fn unavailability_reason(&self) -> Option<String> {
        let missing = self.missing();
        (!missing.is_empty())
            .then(|| format!("transcription unavailable: no {}", missing.join(", ")))
    }
}

/// Look `name` up on `PATH`, returning the first candidate that exists
/// as a file.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// What one drain pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Pending rows the pass attempted.
    pub attempted: u64,
    /// Rows that gained a transcript (and are now indexed for search).
    pub transcribed: u64,
    /// Rows that failed (missing bytes, failed conversion, failed
    /// transcription). They stay `pending` and are retried by the
    /// next drain.
    pub failed: u64,
    /// Why the most recent skipped row was skipped — a debugging handle
    /// for the follow-up wiring's status surface, not a per-row log.
    pub last_skip_reason: Option<String>,
    /// Present iff the sidecar is not usable: the pass attempted
    /// nothing and the pending rows stay untouched.
    pub unavailable: Option<String>,
}

impl DrainOutcome {
    fn unavailable(reason: String) -> Self {
        Self {
            unavailable: Some(reason),
            ..Self::default()
        }
    }
}

/// Enqueue transcription for every preserved audio attachment the
/// projection knows (own transaction). Idempotent: a (message,
/// digest) pair is enqueued once. Returns how many pending rows this
/// call added.
///
/// The import/reconcile hook: the attachment backfill calls this
/// inside its batch transactions when the setting is on, so enqueue
/// rides the single writer of `attachment_refs` and never runs the
/// sidecar itself.
pub fn enqueue_pending_audio(storage: &Storage) -> Result<u64> {
    let tx = storage.connection().unchecked_transaction()?;
    let added = enqueue_in_tx(&tx)?;
    tx.commit()?;
    Ok(added)
}

/// The set-based enqueue body: one `INSERT … SELECT` over the
/// projection — every `preserved` audio reference without a
/// transcripts row becomes `pending`.
pub(crate) fn enqueue_in_tx(tx: &Transaction<'_>) -> Result<u64> {
    let added = tx.execute(
        "INSERT INTO transcripts (message_id, sha256)
         SELECT DISTINCT r.message_id, r.sha256
         FROM attachment_refs AS r
         WHERE r.kind = 'audio'
           AND r.availability = 'preserved'
           AND r.sha256 IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM transcripts AS t
               WHERE t.message_id = r.message_id AND t.sha256 = r.sha256
           )",
        [],
    )? as u64;
    Ok(added)
}

/// The pending queue, oldest first (what [`drain_pending`] walks).
fn pending_rows(conn: &Connection, limit: u64) -> Result<Vec<PendingTranscript>> {
    let mut stmt = conn.prepare(
        "SELECT id, message_id, sha256 FROM transcripts
         WHERE status = 'pending' ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(PendingTranscript {
                id: row.get(0)?,
                message_id: row.get(1)?,
                sha256: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// How many transcripts rows are still `pending`.
pub fn pending_count(storage: &Storage) -> Result<u64> {
    let count: i64 = storage.connection().query_row(
        "SELECT COUNT(*) FROM transcripts WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// One message's transcript rows, for the reading pane's transcript
/// text and the voice badge (`status`-agnostic: the UI slice decides
/// presentation).
pub fn message_transcripts(storage: &Storage, message_id: i64) -> Result<Vec<TranscriptRow>> {
    let conn = storage.connection();
    let mut stmt = conn.prepare(
        "SELECT message_id, sha256, status, language, text, model, transcribed_at
         FROM transcripts WHERE message_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![message_id], |row| {
            Ok(TranscriptRow {
                message_id: row.get(0)?,
                sha256: row.get(1)?,
                status: row.get(2)?,
                language: row.get(3)?,
                text: row.get(4)?,
                model: row.get(5)?,
                transcribed_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Drain up to `limit` pending transcripts through the sidecar.
///
/// One row per transaction: a crash keeps every transcript that
/// landed. Per-row failures (bytes missing from the store, ffmpeg or
/// the sidecar failing, no parsable text) count into
/// [`DrainOutcome::failed`] and leave the row `pending` — the next
/// drain retries it; nothing ever panics or fails the pass. When
/// `caps` is incomplete the pass reports honest unavailability and
/// does nothing.
pub fn drain_pending(
    storage: &Storage,
    media_dir: &Path,
    caps: &TranscriptionCaps,
    limit: u64,
) -> Result<DrainOutcome> {
    // Honest unavailability: with any tool absent the pass attempts
    // nothing and every pending row stays untouched.
    if let Some(reason) = caps.unavailability_reason() {
        return Ok(DrainOutcome::unavailable(reason));
    }
    // Every tool present: proceed with the resolved paths (the
    // unwraps are guaranteed by `missing()` being empty).
    let whisper = caps.whisper_bin.clone().unwrap();
    let ffmpeg = caps.ffmpeg_bin.clone().unwrap();
    let model = caps.model.clone().unwrap();
    let model_tag = model
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("whisper")
        .to_string();

    let rows = pending_rows(storage.connection(), limit)?;
    let mut outcome = DrainOutcome {
        attempted: rows.len() as u64,
        ..DrainOutcome::default()
    };
    for row in rows {
        match transcribe_one(
            storage, media_dir, &whisper, &ffmpeg, &model, &model_tag, &row,
        ) {
            Ok(()) => outcome.transcribed += 1,
            Err(SidecarError::Skipped(reason)) => {
                outcome.failed += 1;
                outcome.last_skip_reason = Some(reason);
            }
            Err(SidecarError::Fatal(e)) => return Err(e),
        }
    }
    Ok(outcome)
}

/// The per-row error split: sidecar failures skip-and-continue, vault
/// write failures are fatal to the pass.
enum SidecarError {
    /// Skip this row (it stays `pending`); the string explains why for
    /// debugging.
    Skipped(String),
    /// A vault error that must abort the pass.
    Fatal(crate::Error),
}

impl From<crate::Error> for SidecarError {
    fn from(e: crate::Error) -> Self {
        SidecarError::Fatal(e)
    }
}

impl From<rusqlite::Error> for SidecarError {
    fn from(e: rusqlite::Error) -> Self {
        SidecarError::Fatal(e.into())
    }
}

/// Transcribe one pending row end to end. All sidecar interaction
/// failures are [`SidecarError::Skipped`]; only database writes can
/// abort the pass.
fn transcribe_one(
    storage: &Storage,
    media_dir: &Path,
    whisper: &Path,
    ffmpeg: &Path,
    model: &Path,
    model_tag: &str,
    row: &PendingTranscript,
) -> std::result::Result<(), SidecarError> {
    let Some(object) = find_media_object(media_dir, &row.sha256)? else {
        return Err(SidecarError::Skipped(format!(
            "bytes for {} not in the store",
            row.sha256
        )));
    };

    // 16 kHz mono WAV conversion in a scratch dir that cleans itself
    // up; whisper.cpp takes PCM WAV input.
    let scratch = tempfile::tempdir().map_err(|e| {
        SidecarError::Skipped(format!("cannot create scratch dir for {}: {e}", row.sha256))
    })?;
    let wav = scratch.path().join("input.wav");
    let convert = Command::new(ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(&object)
        .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(&wav)
        .output()
        .map_err(|e| SidecarError::Skipped(format!("ffmpeg failed to start: {e}")))?;
    if !convert.status.success() || !wav.is_file() {
        return Err(SidecarError::Skipped(format!(
            "ffmpeg could not convert {}",
            row.sha256
        )));
    }

    let spoken = Command::new(whisper)
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(&wav)
        .output()
        .map_err(|e| SidecarError::Skipped(format!("whisper failed to start: {e}")))?;
    if !spoken.status.success() {
        return Err(SidecarError::Skipped(format!(
            "whisper exited with {} for {}",
            spoken.status, row.sha256
        )));
    }
    let stdout = String::from_utf8_lossy(&spoken.stdout);
    let mut text = parse_whisper_text(&stdout);
    if text.is_empty() {
        // Some whisper.cpp builds print segments on stderr.
        let stderr = String::from_utf8_lossy(&spoken.stderr);
        text = parse_whisper_text(&stderr);
    }
    if text.is_empty() {
        return Err(SidecarError::Skipped(format!(
            "whisper produced no text for {}",
            row.sha256
        )));
    }

    // Land the transcript and refresh the message's FTS document in
    // one transaction: the transcripts row is the source of truth,
    // the concatenated done texts are what the update trigger indexes.
    let conn = storage.connection();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE transcripts
         SET status = 'done', text = ?2, model = ?3,
             transcribed_at = datetime('now')
         WHERE id = ?1",
        params![row.id, text, model_tag],
    )?;
    tx.execute(
        "UPDATE archive_messages
         SET transcript = (
             SELECT COALESCE(group_concat(text, ' '), '')
             FROM transcripts
             WHERE message_id = archive_messages.id AND status = 'done'
         )
         WHERE id = ?1",
        params![row.message_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// The stored object for `sha256`: the store names objects
/// `media/<sha256>.<ext>`, and the extension is whatever the export
/// carried, so match the digest prefix (the staging dir under
/// `media/.staging` never matches — its entries sit below it).
fn find_media_object(media_dir: &Path, sha256: &str) -> Result<Option<PathBuf>> {
    let prefix = format!("{sha256}.");
    let entries = std::fs::read_dir(media_dir)?;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Extract the spoken text from whisper.cpp CLI output: the segment
/// lines the CLI prints — `[start --> end] spoken text` — keep only
/// their payload. Everything else (the loader and timing logs the CLI
/// also prints, on stderr especially) must never become transcript
/// text; manual verification against a real whisper-cli found those
/// log lines on stderr with no segments at all for a tone input, and
/// the pass below must report "no text" there, not harvest the logs.
/// Stub scripts print the same segment shape.
fn parse_whisper_text(raw: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if !(trimmed.starts_with('[') && trimmed.contains(" --> ")) {
            continue;
        }
        if let Some(idx) = trimmed.find("] ") {
            let payload = trimmed[idx + 2..].trim();
            if !payload.is_empty() {
                parts.push(payload);
            }
        }
    }
    parts.join(" ")
}
