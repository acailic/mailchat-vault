//! Bounded streaming ingest (M2.2): parser interfaces that never hold a
//! whole archive in memory, plus the batch-commit driver that turns a
//! [`StreamSource`] into committed rows under a fixed memory budget.
//!
//! # Why
//!
//! The pre-M2.2 production path was [`crate::ingest::Source::collect`]:
//! every parser read the entire export into a `Vec<NewMessage>` before
//! the first row was written. A multi-gigabyte Telegram export (or a
//! Discord package of many channels) therefore cost its full size in
//! heap, inside the writer lane, before any progress was visible.
//!
//! # The model
//!
//! A [`StreamSource`] yields one [`StreamItem`] at a time to a callback
//! ([`StreamSource::stream`]); the parser keeps at most one item plus its
//! own bookkeeping alive (a line buffer, a JSON element). The one
//! exception is the content-hash id disambiguators' occurrence counter,
//! which keeps one entry per *distinct* generated id and so grows
//! linearly (but slowly — a few dozen bytes per message) with the
//! archive. Parsers may re-read their input in bounded passes
//! (WhatsApp needs the participant set before ids can be hashed; JSON
//! exports read their header first) — passes stream too, so memory stays
//! flat regardless of file size.
//!
//! [`BoundedRun`] drives a source against an [`IngestSink`]:
//!
//! - **Batched commits** — messages and issue records accumulate until
//!   [`StreamLimits`] is reached (at most 200 messages or 8 MiB of
//!   normalized text, whichever comes first — the architecture's starting
//!   limits), then land in **one transaction** together with the import
//!   checkpoint. A batch is all-or-nothing; the stream between batches
//!   holds at most one batch plus one item.
//! - **Cancellation** — cooperative, checked per item: a cancel stops the
//!   pull, *discards the uncommitted partial batch*, and returns the
//!   committed counts unchanged (cancellation preserves committed data
//!   and correct counts by construction — anything reported was already
//!   committed).
//! - **Issue records** — parse failures are not dropped: each becomes a
//!   durable `import_issues` row (position, best-effort external ref,
//!   reason) committed with the batch that reached it.
//! - **Restart recovery** — every batch advances `committed_items`; an
//!   interrupted run (crash, cancel, injected failure) leaves the
//!   checkpoint at the last committed position, and the next run of the
//!   same import id fast-forwards the stream past those items
//!   ([`StreamSource::stream`]'s `skip`) and continues — no duplicates
//!   (ids are deterministic, and the skipped items were already
//!   committed), no re-parse of committed history, no lost failures (a
//!   skipped failure was already recorded with its batch). Completing an
//!   import deletes its checkpoint in the same transaction as the last
//!   batch: only genuinely interrupted imports leave recovery state
//!   behind.
//!
//! # Where it runs
//!
//! Parsing, hashing, and media copying happen on the caller's thread
//! (the job runner, or a Tauri blocking task) — never inside a storage
//! lock or transaction. Each batch's commit round-trips the pool's
//! bounded write queue ([`crate::jobs::PoolSink`]) or, in tests, goes
//! straight to [`crate::storage::Storage`] (which implements
//! [`IngestSink`] directly).
//!
//! The whole-archive [`crate::ingest::Source::collect`] helpers remain
//! only as a convenience for small parser tests.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ingest::IngestSummary;
use crate::media::MediaSummary;
use crate::message::NewMessage;
use crate::object_store::Capacity;
use crate::storage::ImportIssueRecord;
use crate::Result;

/// Progress events are rate-limited to at most four per second (the
/// architecture's ceiling for job progress publication).
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// One item parsed from an export: a message, or a durable parse failure.
///
/// The message variant dominates the enum's size, but only one item is
/// ever alive at a time in the streaming paths (the driver's batch is a
/// separate `Vec`), so boxing would add indirection to the hot path for
/// no memory-budget benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq)]
pub enum StreamItem {
    Message(NewMessage),
    /// An entry the parser encountered but could not turn into a message
    /// (malformed entry, missing id, unparseable mail). The driver turns
    /// these into issue records; `external_ref` is the best-effort
    /// upstream reference (empty when the entry carried none).
    Failed {
        external_ref: String,
        reason: String,
    },
}

/// Parser-internal outcome for one input entry, before the driver sees
/// it: the shared shape every streaming converter returns.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum EntryOutcome {
    Message(NewMessage),
    /// The entry could not be turned into a message; becomes an issue
    /// record (`external_ref` is the best-effort upstream reference).
    Failed {
        external_ref: String,
        reason: String,
    },
    /// Silently skipped entry (service events, empty parts): not an item
    /// at all — neither emitted, nor counted, nor a skip-position step.
    Skip,
}

/// The shared tail of every streaming parser: push one converted entry
/// through the skip counter (restart recovery) and the consumer. A
/// skipped entry advances the counter without emitting — its fate was
/// committed by the earlier run.
pub(crate) fn emit_entry(
    outcome: EntryOutcome,
    skip: &mut u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
) -> Result<Flow> {
    match outcome {
        EntryOutcome::Skip => Ok(Flow::Continue),
        EntryOutcome::Message(msg) => {
            if *skip > 0 {
                *skip -= 1;
                return Ok(Flow::Continue);
            }
            emit(StreamItem::Message(msg))
        }
        EntryOutcome::Failed {
            external_ref,
            reason,
        } => {
            if *skip > 0 {
                *skip -= 1;
                return Ok(Flow::Continue);
            }
            emit(StreamItem::Failed {
                external_ref,
                reason,
            })
        }
    }
}

/// Shared state for streaming parser visitors: the skip counter and the
/// consumer callback, plus out-of-band stop/error capture. Serde's error
/// channel is `serde_json::Error` (string-typed), which would flatten a
/// storage error from the consumer into a message; the slot carries the
/// original error out intact, and `stopped` records a clean early stop.
pub(crate) struct ConsumerState<'a> {
    pub skip: u64,
    pub emit: &'a mut dyn FnMut(StreamItem) -> Result<Flow>,
    pub stopped: bool,
    pub failure: &'a mut Option<crate::Error>,
}

impl<'a> ConsumerState<'a> {
    /// Offer one converted entry to the consumer; `true` = keep reading.
    /// A stop or error is recorded and reflected as `false` — the visitor
    /// then aborts with a sentinel error (either kind), which the parser
    /// re-raises from its slot (failure) or recognizes as a clean stop.
    pub fn offer(&mut self, outcome: EntryOutcome) -> bool {
        match emit_entry(outcome, &mut self.skip, self.emit) {
            Ok(Flow::Continue) => true,
            Ok(Flow::Stop) => {
                self.stopped = true;
                false
            }
            Err(e) => {
                *self.failure = Some(e);
                false
            }
        }
    }

    /// The sentinel used to abort a serde walk on a clean consumer stop.
    /// An early `Ok` from a visitor does **not** work: serde_json still
    /// runs its end-of-container scan (`end_map`/`end_seq`) and fails on
    /// "trailing characters" because the walk sits mid-value. A sentinel
    /// error aborts immediately and the call site recognizes it (a
    /// consumer *failure* needs no distinct sentinel: its slot is checked
    /// first and carries the original error intact).
    pub fn stop<E: serde::de::Error>() -> E {
        E::custom(STOP_SENTINEL)
    }

    /// Whether `e` is the clean-stop sentinel (only consulted when
    /// `stopped` is set, so a genuine parse error is never mistaken).
    pub fn is_stop_sentinel(e: &serde_json::Error) -> bool {
        e.to_string().contains(STOP_SENTINEL)
    }
}

const STOP_SENTINEL: &str = "stream stopped by consumer";

/// What the consumer wants after receiving one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep streaming.
    Continue,
    /// Stop pulling items promptly (cancellation). The parser unwinds
    /// without reading further entries; already-emitted items stay
    /// emitted.
    Stop,
}

/// A parser that yields items one at a time with bounded memory.
///
/// `stream` visits every item of the input in order and hands it to
/// `emit`. `skip` discards the first `skip` items **without emitting
/// them** — restart recovery fast-forwards past items whose fate was
/// already committed (skipping counts messages and failures alike;
/// entries the parser skips silently — service events, empty parts —
/// never reach `emit` in the first place). The stream must be
/// deterministic for a given input: the same file yields the same items
/// in the same order on every pass, which is what makes `skip` safe.
///
/// Production code consumes sources through [`BoundedRun`]; the
/// [`crate::ingest::Source::collect`] whole-archive helpers stay for
/// small parser tests only.
pub trait StreamSource {
    /// Stable source name used as the dedup namespace (e.g. `"mbox"`).
    fn name(&self) -> &str;

    /// Stream items to `emit`, discarding the first `skip` of them.
    /// Returning [`Flow::Stop`] from `emit` stops the stream early with
    /// `Ok(())`. Errors are structural (unreadable input, invalid JSON)
    /// and abort the whole stream.
    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()>;
}

/// Batch-commit limits: a batch flushes when either ceiling is hit.
///
/// Defaults are the architecture's starting limits (validate against the
/// performance harness): **200 messages or 8 MiB of normalized text per
/// commit, whichever comes first**. Tests shrink them to exercise the
/// boundaries. A single item larger than the whole budget still commits
/// alone — oversized items are an explicit capacity question (see the
/// media/capacity contracts), never a silent truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    /// Maximum messages accumulated before a commit.
    pub max_messages_per_batch: usize,
    /// Maximum accumulated body text (bytes) before a commit.
    pub max_batch_text_bytes: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_messages_per_batch: 200,
            max_batch_text_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Which export flavor the media pass should run for a bounded import
/// (see [`crate::media`]; Discord records attachments in `raw_json`
/// only, so it has no media pass yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Telegram,
    WhatsApp,
    /// Signal conversation exports: attachment `fileName`s in `raw_json`
    /// carry no paths — the bytes sit somewhere in the export tree — so
    /// the pass matches names against a bounded walk of it.
    Signal,
    /// Email (mbox): the parser decoded MIME attachment/inline parts into
    /// [`NewMessage::attachments`]; the pass stages their bytes into the
    /// object store and records `media` entries before the batch commits.
    Mail,
    /// Facebook Messenger (DYI tree): media uris in `raw_json`
    /// (`photos`/`videos`/… arrays, the single-object `sticker`) are
    /// root-relative and resolve against the DYI export root.
    Messenger,
}

/// The media pass configuration of one bounded run (M4.2): which
/// extraction flavor runs, where the export tree and the vault's app
/// data live, and whether unlinked export files are stored too.
#[derive(Debug, Clone, Copy)]
pub struct MediaPass<'a> {
    pub kind: MediaKind,
    /// The directory the export's media files live under (the export
    /// file's parent for chat exports; unused for mail).
    pub export_root: &'a Path,
    /// The export file itself (never stored as an unlinked file).
    pub export_file: Option<&'a Path>,
    /// The vault's app-data dir (the object store lives under
    /// `<data_dir>/media/`).
    pub data_dir: &'a Path,
    /// Store export-tree files no message references (M4.2) — enabled
    /// only for imports that picked the whole tree (directory or `.zip`)
    /// and only for the chat kinds that have a media pass.
    pub unlinked: bool,
}

impl<'a> MediaPass<'a> {
    /// The mail media pass (mbox): attachment bytes are already decoded
    /// onto the messages, so there is no export tree to walk — the
    /// `export_root`/`export_file`/`unlinked` fields are dead state for
    /// mail and stay at their documented inert values (`"."`, `None`,
    /// `false`) so one struct serves every kind (M4.2; see the field
    /// docs). The single construction site for the two src-tauri mbox
    /// paths (the command and the job) so the literals cannot drift.
    pub fn mail(data_dir: &'a Path) -> Self {
        Self {
            kind: MediaKind::Mail,
            export_root: Path::new("."),
            export_file: None,
            data_dir,
            unlinked: false,
        }
    }
}

/// Storage access for [`BoundedRun`]: load a checkpoint, commit a batch.
///
/// Implemented directly by [`crate::storage::Storage`] (tests, the
/// writer's own closures) and by [`crate::jobs::PoolSink`] (production:
/// each commit round-trips the pool's bounded write queue, so parsing
/// never holds the writer while it reads).
pub trait IngestSink {
    /// The committed item count of a previously interrupted run of this
    /// import (`None` = no recovery state; a completed import leaves
    /// none).
    fn checkpoint(&mut self, import_id: &str) -> Result<Option<u64>>;
    /// Commit one batch atomically: message rows (with dedup and media
    /// backfill), issue records, unlinked-object registry rows, and the
    /// checkpoint advance — or, when `completes` is set, the checkpoint
    /// removal that finishes the import.
    #[allow(clippy::too_many_arguments)]
    fn commit_batch(
        &mut self,
        import_id: &str,
        source: &str,
        messages: &[NewMessage],
        issues: &[ImportIssueRecord<'_>],
        media_objects: &[crate::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> Result<crate::storage::ImportBatchCounts>;

    /// The digests every stored message references through its `raw_json`
    /// `media` entries (the message-linked side of the reference set; the
    /// M4.2 unlinked pass skips these files — they are already preserved
    /// and reachable from their messages).
    fn linked_media_digests(&mut self) -> Result<HashSet<String>>;
}

/// The outcome of one bounded import run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedOutcome {
    /// Counts of **this run** (committed batches only): `seen ==
    /// inserted + skipped + failed`. A resumed run counts its own
    /// portion; the interrupted earlier run reported its own.
    pub summary: IngestSummary,
    /// Media-pass counters, present when extraction ran.
    pub media: Option<MediaSummary>,
    /// The run's stable import identity (checkpoints, issue records,
    /// and the import-scope filter key on it).
    pub import_id: String,
    /// Cancellation was observed and the run stopped early. Everything
    /// reported in `summary` was committed; the uncommitted partial
    /// batch was discarded; the checkpoint keeps the resume position.
    pub cancelled: bool,
    /// Items this run skipped because a previous run already committed
    /// them (restart recovery).
    pub resumed_from: u64,
    /// Total items whose fate is durable after this run (the checkpoint
    /// value a later run would resume from).
    pub committed_items: u64,
}

/// One bounded import: identity, limits, and the optional media pass.
#[derive(Debug, Clone)]
pub struct BoundedRun<'a> {
    /// Stable import identity: checkpoints and issue records key on it.
    /// Same input file ⇒ same id ⇒ interrupted runs resume; a changed
    /// file must produce a different id (see [`import_id_for`]).
    pub import_id: String,
    pub limits: StreamLimits,
    /// Run the media extraction pass for this export kind before each
    /// commit (file copies happen on the driver thread, outside every
    /// storage lock).
    pub media: Option<MediaPass<'a>>,
}

impl<'a> BoundedRun<'a> {
    /// A run with default limits and no media pass.
    pub fn new(import_id: impl Into<String>) -> Self {
        Self {
            import_id: import_id.into(),
            limits: StreamLimits::default(),
            media: None,
        }
    }

    /// Drive `source` to completion (or cancellation) against `sink`.
    ///
    /// `is_cancelled` is polled once per item and between batches;
    /// `progress(done, total, message)` is called after every commit and
    /// at start, rate-limited to four events per second (`total` is `0`:
    /// export formats do not declare totals, and unknown totals are
    /// never guessed).
    pub fn run(
        &self,
        source: &dyn StreamSource,
        sink: &mut dyn IngestSink,
        is_cancelled: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(u64, u64, &str),
    ) -> Result<BoundedOutcome> {
        let resumed_from = sink.checkpoint(&self.import_id)?.unwrap_or(0);
        let source_name = source.name().to_string();
        let mut run = RunState {
            sink,
            import_id: &self.import_id,
            source_name: &source_name,
            media: self.media,
            media_budget: Capacity::default(),
            summary: IngestSummary {
                source: source_name.clone(),
                seen: 0,
                inserted: 0,
                skipped: 0,
                failed: 0,
            },
            media_summary: self.media.map(|_| MediaSummary::default()),
            batch: Vec::new(),
            issues: Vec::new(),
            batch_text_bytes: 0,
            committed_items: resumed_from,
            next_position: resumed_from,
            cancelled: false,
            consumed_paths: HashSet::new(),
            unlinked_objects: Vec::new(),
        };
        let started = Instant::now();
        let mut last_progress = None::<Instant>;
        let mut emit_progress = |done: u64, last: bool| {
            let due = match last_progress {
                None => true,
                Some(at) => last || started.saturating_duration_since(at) >= PROGRESS_INTERVAL,
            };
            if due {
                last_progress = Some(Instant::now());
                progress(done, 0, "");
            }
        };
        emit_progress(run.committed_items, false);

        source.stream(resumed_from, &mut |item| {
            if is_cancelled() {
                run.cancelled = true;
                return Ok(Flow::Stop);
            }
            let position = run.next_position;
            run.next_position += 1;
            match item {
                StreamItem::Message(msg) => {
                    // Attachment bytes count against the batch budget
                    // alongside body text, so a batch of media-heavy
                    // mails flushes early instead of accumulating
                    // (the same contract as text, same limits).
                    run.batch_text_bytes += msg.body.len()
                        + msg.attachments.iter().map(|a| a.bytes.len()).sum::<usize>();
                    run.batch.push(msg);
                }
                StreamItem::Failed {
                    external_ref,
                    reason,
                } => run.issues.push(PendingIssue {
                    position,
                    external_ref,
                    reason,
                }),
            }
            // Flush when either limit is hit (>=, so a single oversized
            // item still commits alone rather than looping).
            if run.batch.len() + run.issues.len() >= self.limits.max_messages_per_batch
                || run.batch_text_bytes >= self.limits.max_batch_text_bytes
            {
                run.commit_batch(false)?;
                emit_progress(run.committed_items, false);
            }
            Ok(Flow::Continue)
        })?;

        if run.cancelled {
            // The partial batch is discarded: nothing uncommitted is
            // reported, and the checkpoint already holds the last
            // committed position for the resume.
            return Ok(run.outcome(resumed_from));
        }
        // Natural end, in dependency order: first the final batch commits
        // — running its message media pass, so every file a message
        // claims is stored and marked consumed — and finishes the import
        // (the checkpoint goes with it). The M4.2 unlinked pass then
        // walks the tree — now every leftover file is genuinely
        // unlinked — and a separate registry commit registers the
        // objects it found; a crash in between leaves stored but
        // unregistered objects, which a re-import of the same tree
        // re-registers.
        run.commit_batch(true)?;
        run.collect_unlinked()?;
        run.commit_registry()?;
        emit_progress(run.committed_items, true);
        Ok(run.outcome(resumed_from))
    }
}

/// The mutable state of one run; `commit_batch` is the only writer of
/// rows. Lives on the driver thread; parsing happens inside the source's
/// `stream` callback below.
struct RunState<'a, 'b> {
    sink: &'b mut dyn IngestSink,
    import_id: &'a str,
    source_name: &'a str,
    media: Option<MediaPass<'a>>,
    /// The M4.1 media capacity budget for this run — shared across every
    /// batch's media pass, so one import cannot store past its bounds;
    /// dedup hits refund their charge (see [`crate::object_store`]).
    media_budget: Capacity,
    summary: IngestSummary,
    media_summary: Option<MediaSummary>,
    batch: Vec<NewMessage>,
    issues: Vec<PendingIssue>,
    batch_text_bytes: usize,
    committed_items: u64,
    next_position: u64,
    cancelled: bool,
    /// Export files the message media passes stored — every file a
    /// message claimed (M4.2; the unlinked pass stores everything else).
    consumed_paths: HashSet<PathBuf>,
    /// Unlinked objects staged for the `media_objects` registry, riding
    /// the final batch's commit.
    unlinked_objects: Vec<crate::media::UnlinkedObject>,
}

/// A parse failure accumulated for the next commit (owned; becomes a
/// borrowed [`ImportIssueRecord`] inside the commit call).
struct PendingIssue {
    position: u64,
    external_ref: String,
    reason: String,
}

impl RunState<'_, '_> {
    /// The M4.2 unlinked pass: after the final batch committed, store
    /// every export file no message claimed — skipping files this run
    /// consumed, files whose content any stored message already links
    /// (resume-safe: an interrupted run's earlier batches linked files
    /// this run never re-walks), the export file itself, and exporter
    /// artifacts — and collect them for the registry. Only chat passes
    /// with the unlinked mode on run this (whole-tree picks: directory
    /// or `.zip`).
    fn collect_unlinked(&mut self) -> Result<()> {
        let Some(pass) = self.media else {
            return Ok(());
        };
        if !pass.unlinked {
            return Ok(());
        }
        let Some(summary) = self.media_summary.as_mut() else {
            return Ok(());
        };
        let linked = self.sink.linked_media_digests()?;
        let records = crate::media::extract_unlinked(
            pass.export_root,
            pass.export_file,
            pass.data_dir,
            &self.consumed_paths,
            &linked,
            summary,
            &mut self.media_budget,
        )?;
        self.unlinked_objects.extend(records);
        Ok(())
    }

    /// The registry commit: an empty batch that carries only the
    /// unlinked objects. The checkpoint is already gone (the final
    /// message batch deleted it); committing empty with `completes` is
    /// idempotent. This is its own transaction after the import
    /// completed — a crash before it lands leaves stored but
    /// unregistered objects, which a re-import re-registers.
    fn commit_registry(&mut self) -> Result<()> {
        if self.unlinked_objects.is_empty() {
            return Ok(());
        }
        self.sink.commit_batch(
            self.import_id,
            self.source_name,
            &[],
            &[],
            &self.unlinked_objects,
            self.committed_items,
            true,
        )?;
        self.unlinked_objects.clear();
        Ok(())
    }

    /// Commit the accumulated batch (media pass first — file copies on
    /// this thread, outside the storage transaction), then reset it.
    /// `completes` marks the import finished.
    fn commit_batch(&mut self, completes: bool) -> Result<()> {
        if let (Some(pass), Some(summary)) = (self.media, self.media_summary.as_mut()) {
            if !self.batch.is_empty() {
                match pass.kind {
                    MediaKind::Telegram => crate::media::extract_telegram(
                        &mut self.batch,
                        pass.export_root,
                        pass.data_dir,
                        summary,
                        &mut self.media_budget,
                        &mut self.consumed_paths,
                    )?,
                    MediaKind::WhatsApp => crate::media::extract_whatsapp(
                        &mut self.batch,
                        pass.export_root,
                        pass.data_dir,
                        summary,
                        &mut self.media_budget,
                        &mut self.consumed_paths,
                    )?,
                    MediaKind::Signal => crate::media::extract_signal(
                        &mut self.batch,
                        pass.export_root,
                        pass.data_dir,
                        summary,
                        &mut self.media_budget,
                        &mut self.consumed_paths,
                    )?,
                    MediaKind::Mail => {
                        let mut mail_media =
                            crate::media::MailMedia::new(pass.data_dir, &mut self.media_budget);
                        for msg in self.batch.iter_mut() {
                            mail_media.store_message(msg);
                        }
                        summary.stored += mail_media.stored;
                        summary.deduped += mail_media.deduped;
                        summary.missing += mail_media.missing;
                    }
                    MediaKind::Messenger => crate::media::extract_messenger(
                        &mut self.batch,
                        pass.export_root,
                        pass.data_dir,
                        summary,
                        &mut self.media_budget,
                        &mut self.consumed_paths,
                    )?,
                }
            }
        }
        let records: Vec<ImportIssueRecord<'_>> = self
            .issues
            .iter()
            .map(|i| ImportIssueRecord {
                position: i.position,
                external_ref: &i.external_ref,
                reason: &i.reason,
            })
            .collect();
        let counts = self.sink.commit_batch(
            self.import_id,
            self.source_name,
            &self.batch,
            &records,
            &[],
            self.committed_items + (self.batch.len() + self.issues.len()) as u64,
            completes,
        )?;
        self.summary.inserted += counts.inserted;
        self.summary.skipped += counts.skipped;
        // Media links are a storage outcome (inserts carrying a `media`
        // array, duplicates that gained entries via backfill), so the
        // sink reports them per batch and the summary accumulates here.
        if let Some(summary) = self.media_summary.as_mut() {
            summary.linked += counts.media_linked;
        }
        self.summary.failed += self.issues.len() as u64;
        self.summary.seen += (self.batch.len() + self.issues.len()) as u64;
        self.committed_items += (self.batch.len() + self.issues.len()) as u64;
        self.batch.clear();
        self.issues.clear();
        self.batch_text_bytes = 0;
        Ok(())
    }

    fn outcome(mut self, resumed_from: u64) -> BoundedOutcome {
        self.summary.source = self.source_name.to_string();
        BoundedOutcome {
            summary: self.summary,
            media: self.media_summary,
            import_id: self.import_id.to_string(),
            cancelled: self.cancelled,
            resumed_from,
            committed_items: self.committed_items,
        }
    }
}

impl IngestSink for crate::storage::Storage {
    fn checkpoint(&mut self, import_id: &str) -> Result<Option<u64>> {
        crate::storage::Storage::import_checkpoint(self, import_id)
    }

    fn commit_batch(
        &mut self,
        import_id: &str,
        source: &str,
        messages: &[NewMessage],
        issues: &[ImportIssueRecord<'_>],
        media_objects: &[crate::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> Result<crate::storage::ImportBatchCounts> {
        crate::storage::Storage::commit_import_batch(
            self,
            import_id,
            source,
            messages,
            issues,
            media_objects,
            committed_after,
            completes,
        )
    }

    fn linked_media_digests(&mut self) -> Result<HashSet<String>> {
        crate::storage::Storage::message_media_digests(self)
    }
}

/// A stable import identity for one input file: its canonical path plus
/// length and modification time. An unchanged file keeps its id across
/// restarts (interrupted runs resume); an edited or replaced file is a
/// new import — its old checkpoint is irrelevant and re-runs dedup
/// safely instead of resuming into shifted positions.
pub fn import_id_for(prefix: &str, path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map_or(0, |m| m.len());
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{prefix}:{}:{}:{}", path.display(), len, mtime)
}
