//! Background jobs and the database pool (M2.1): one dedicated writer
//! thread, two read connections, bounded queues, typed job states.
//!
//! # Architecture
//!
//! [`DbPool`] opens the vault file through several SQLite connections in
//! WAL mode — every connection initializes itself with the same pragmas
//! and custom functions ([`crate::storage::Storage::open`]) — and runs:
//!
//! - a **writer worker thread** owning the one write connection. Writes
//!   are submitted as closures over a **bounded queue**
//!   ([`DbPool::submit_write`]): the queue's capacity is the backpressure
//!   bound, so a burst of write work can never grow memory without limit,
//!   and callers wait for their turn instead of racing on the file.
//!   Exactly one thread ever writes, which is SQLite's preferred model
//!   (single writer, WAL).
//! - **two read connections**, each behind its own mutex, so two reads can
//!   run concurrently while the writer commits: a search never waits for
//!   an import, an IMAP fetch, or an embedding run ([`DbPool::with_read`]).
//! - a **job runner thread** executing background jobs
//!   ([`DbPool::submit_job`]). A job body never touches a connection
//!   directly: it works through a [`JobContext`] whose `read`/`write`
//!   calls round-trip to the pool, so long jobs (network sync, embedding)
//!   cannot occupy the write lane or a read slot while they wait on the
//!   network. This is the structural guarantee behind the M2.1 contract:
//!   **no network operation ever runs inside the writer, inside a storage
//!   lock, or inside a transaction.**
//!
//! # Typed job states and errors
//!
//! Every job carries a stable [`JobId`] and moves through
//! [`JobState::Queued`] → [`JobState::Running`] → a terminal
//! `Succeeded { result }` / `Failed { error }` / `Cancelled`, with typed
//! [`JobError`] kinds (`storage`, `io`, `import`, `network`, `cancelled`,
//! `queue_full`, `invalid`, `internal`) instead of bare strings.
//! Cancellation is cooperative: [`DbPool::cancel_job`] sets a flag the job
//! body observes via [`JobContext::is_cancelled`] at its safe points.
//!
//! # Event log and polling recovery (v2 protocol)
//!
//! Every state change appends a [`JobEvent`] stamped with a global
//! monotonically-increasing `seq`. The Tauri shell forwards each event as
//! a `job-event` window event; a UI that missed events (webview reload, a
//! dropped event) recovers by polling [`DbPool::events_since`] with its
//! last seen `seq`: the page returns everything newer, plus the current
//! job snapshot and a `trimmed` flag telling the caller that events it
//! asked about were already dropped from the bounded ring — the client
//! then reconciles from the snapshot instead of the timeline.
//!
//! # Finished-jobs ledger (restart persistence)
//!
//! The registry itself is in-memory by design (job ids, seq numbers, and
//! cancel flags are process-local), so a restart would leave Activity
//! blank over a vault that just imported thousands of messages. Every
//! terminal transition therefore also appends one row to the
//! `job_ledger` table (migration 16, capped at the newest
//! [`JOB_LEDGER_KEEP`][crate::storage::JOB_LEDGER_KEEP] rows): kind,
//! times, state, a one-line result summary, and the serialized terminal
//! state. On spawn the ledger is loaded into the registry as read-only
//! historical records under synthetic ids ([`RESTORED_JOB_ID_BASE`]) —
//! they ride along in every snapshot and render through the same
//! describeJobState path as live rows, but they are never cancellable
//! and never counted as this session's jobs.
//!
//! This module has no UI dependencies; the Tauri shell maps the pool onto
//! `job_*` IPC commands and `job-event` window events.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bounded::IngestSink;
use crate::imap::{ImapConfig, SyncFailure, SyncStorage};
use crate::message::NewMessage;
use crate::storage::{
    BatchFailure, ConnectorIdentity, ImportBatchCounts, ImportIssueRecord, JobLedgerEntry,
    JobLedgerRow, MailBatchItem, MailBatchOutcome, NewSourceInstance, Occurrence, Storage,
    JOB_LEDGER_KEEP,
};
use crate::{Error, Result};

/// A background job's stable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub u64);

/// Tuning knobs of the pool. Defaults fit the desktop app; tests shrink
/// the rings and queues to exercise the bounds.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Capacity of the bounded write queue: writes beyond this wait for
    /// the writer to drain (backpressure, never unbounded memory).
    pub write_queue_capacity: usize,
    /// Capacity of the bounded job queue: submissions beyond this wait
    /// (or fail fast with [`JobError::QueueFull`] via
    /// [`DbPool::try_submit_job`]).
    pub job_queue_capacity: usize,
    /// How many recent [`JobEvent`]s the ring keeps for polling recovery.
    pub event_ring: usize,
    /// How many finished [`JobRecord`]s stay queryable after they end.
    pub finished_keep: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            write_queue_capacity: 64,
            job_queue_capacity: 64,
            event_ring: 1024,
            finished_keep: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// Typed job model
// ---------------------------------------------------------------------------

/// Where a job currently is, with its terminal payload inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    /// Accepted, waiting for the runner thread.
    Queued,
    /// Executing on the runner thread.
    Running,
    /// Finished; `result` is the job's JSON summary.
    Succeeded { result: Value },
    /// Finished with a typed error.
    Failed { error: JobError },
    /// Cancelled before or during execution (cooperative).
    Cancelled,
}

impl JobState {
    /// Whether the state is terminal (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Succeeded { .. } | JobState::Failed { .. } | JobState::Cancelled
        )
    }
}

/// A typed job failure: the kind classifies the failure, `message` carries
/// the detail. Serialized as `{"kind": ..., ...}` for the v2 protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobError {
    /// SQLite / archive errors.
    Storage { message: String },
    /// Local filesystem errors.
    Io { message: String },
    /// Parsing or ingest errors.
    Import { message: String },
    /// Remote-side failures (IMAP/TLS/LLM round trips).
    Network { message: String },
    /// The job was cancelled.
    Cancelled,
    /// The bounded queue was full and the caller asked not to wait.
    QueueFull { queue: String },
    /// Invalid request (unknown kind, bad parameters).
    Invalid { message: String },
    /// Bug-grade failures (writer/runner stopped, panicked closures).
    Internal { message: String },
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Storage { message }
            | JobError::Io { message }
            | JobError::Import { message }
            | JobError::Network { message }
            | JobError::Invalid { message }
            | JobError::Internal { message } => write!(f, "{message}"),
            JobError::Cancelled => write!(f, "cancelled"),
            JobError::QueueFull { queue } => write!(f, "{queue} queue is full"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<Error> for JobError {
    fn from(e: Error) -> Self {
        match e {
            Error::Sqlite(_)
            | Error::UnsafeMigration(_)
            | Error::MigrationValidationFailed(_)
            | Error::MigrationFailed { .. }
            | Error::InsufficientSpace { .. }
            | Error::SchemaTooNew { .. }
            | Error::MigrationFault(_)
            | Error::Backup(_) => JobError::Storage {
                message: e.to_string(),
            },
            Error::Io(_) => JobError::Io {
                message: e.to_string(),
            },
            Error::MailParse(_)
            | Error::ChatImport(_)
            | Error::ImportBounds(_)
            | Error::InvalidQuery(_)
            | Error::InvalidDate(_)
            | Error::InvalidExport(_)
            | Error::InvalidCollection(_)
            | Error::Media(_)
            | Error::Json(_) => JobError::Import {
                message: e.to_string(),
            },
            Error::Imap(_) | Error::Tls(_) | Error::InvalidServerName(_) | Error::Llm(_) => {
                JobError::Network {
                    message: e.to_string(),
                }
            }
            Error::Settings(_) | Error::Embedding(_) | Error::Jobs(_) => JobError::Invalid {
                message: e.to_string(),
            },
            Error::PersonNotFound(_)
            | Error::InvalidPersonInput(_)
            | Error::IdentityLinkConflict { .. }
            | Error::IdentityLinkNotFound(_) => JobError::Invalid {
                message: e.to_string(),
            },
            Error::OAuth(_) => JobError::Network {
                message: e.to_string(),
            },
        }
    }
}

/// Cooperative progress a running job last reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub done: u64,
    pub total: u64,
    /// Short human hint (e.g. the folder being synced); empty = none.
    pub message: String,
}

/// A job's current record — the authoritative state behind `job_get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    /// Job kind (e.g. `"import_mbox"`, `"imap_sync"`).
    pub kind: String,
    pub state: JobState,
    /// RFC 3339 submission time.
    pub submitted_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Last reported progress of a running job (kept on terminal states).
    pub progress: Option<JobProgress>,
    /// True only for the read-only historical rows boot recovery loads
    /// from the finished-jobs ledger (migration 16): a previous session's
    /// job, listed for context, never cancellable and never confused with
    /// this session's counters. Off the wire for every live record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
}

/// Restored ledger rows surface under synthetic ids based here: far above
/// any process-local id this session can allocate (so live and historical
/// records never collide in an id-keyed client map), and far below
/// JavaScript's 2^53 safe-integer boundary. Within the restored set the
/// newest row gets the highest id, so "order by id descending" keeps
/// listing newest first across the live/historical seam.
pub const RESTORED_JOB_ID_BASE: u64 = 1_000_000_000_000;

/// One state change of one job, stamped with a global sequence number.
///
/// `seq` values are allocated from one monotonic counter and never reused,
/// so a client holding the largest `seq` it has seen can ask
/// [`DbPool::events_since`] for exactly what it missed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEvent {
    pub seq: u64,
    /// RFC 3339 time of the change.
    pub at: String,
    pub job: JobId,
    /// Flattened into the event object: the wire form is
    /// `{seq, at, job, type, …}` — `type` tags the [`JobEventDetail`]
    /// variant, matching the frontend protocol types (src/types.ts).
    #[serde(flatten)]
    pub detail: JobEventDetail,
}

/// What happened to a job (the timeline counterpart of [`JobState`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEventDetail {
    /// The job entered the queue (carries its kind so a client seeing
    /// only events can label it without a snapshot round trip).
    Submitted {
        kind: String,
    },
    Started,
    Progress {
        done: u64,
        total: u64,
        message: String,
    },
    Succeeded {
        result: Value,
    },
    Failed {
        error: JobError,
    },
    Cancelled,
}

/// The polling-recovery page: everything after a cursor, plus the current
/// snapshot. `trimmed` tells the caller the ring no longer holds some
/// events newer than its cursor — reconcile from `jobs` then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEventsPage {
    /// Events with `seq > cursor`, ascending.
    pub events: Vec<JobEvent>,
    /// The newest `seq` allocated so far (the next cursor).
    pub latest_seq: u64,
    /// The oldest `seq` still kept in the ring (`next_seq` when empty).
    pub oldest_kept: u64,
    /// Whether events between the caller's cursor and `oldest_kept` were
    /// dropped before this poll (ring capacity exceeded).
    pub trimmed: bool,
    /// Current records: active jobs plus recently finished ones, plus —
    /// after a restart — the ledger's restored history (marked
    /// `restored`, newest last in the vector, ordered below every live
    /// id).
    pub jobs: Vec<JobRecord>,
}

// ---------------------------------------------------------------------------
// Registry (shared state behind one mutex)
// ---------------------------------------------------------------------------

struct JobRegistry {
    next_id: u64,
    next_seq: u64,
    records: Vec<JobRecord>,
    /// Read-only historical rows loaded from the finished-jobs ledger at
    /// spawn (newest first, synthetic ids below every live id). They are
    /// never mutated, never cancelled, and ride along in every snapshot
    /// the client polls — see [`DbPool::spawn`].
    restored: Vec<JobRecord>,
    events: VecDeque<JobEvent>,
    cancels: HashMap<JobId, Arc<AtomicBool>>,
    finished_order: Vec<JobId>,
    finished_keep: usize,
    event_ring: usize,
}

impl JobRegistry {
    /// A registry seeded with the ledger's finished rows (newest first).
    /// Live ids start above the restored band, so id-descending order
    /// lists this session's jobs first and the history below them, and no
    /// live record can ever collide with a restored one.
    fn with_restored(config: &PoolConfig, restored: Vec<JobRecord>) -> Self {
        let next_id = if restored.is_empty() {
            1
        } else {
            RESTORED_JOB_ID_BASE + 1
        };
        Self {
            next_id,
            next_seq: 1,
            records: Vec::new(),
            restored,
            events: VecDeque::new(),
            cancels: HashMap::new(),
            finished_order: Vec::new(),
            finished_keep: config.finished_keep,
            event_ring: config.event_ring,
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    fn register(&mut self, kind: String) -> (JobId, JobEvent) {
        let id = JobId(self.next_id);
        self.next_id += 1;
        let submitted_at = Self::now();
        let event = JobEvent {
            seq: self.next_seq(),
            at: submitted_at.clone(),
            job: id,
            detail: JobEventDetail::Submitted { kind: kind.clone() },
        };
        self.records.push(JobRecord {
            id,
            kind,
            state: JobState::Queued,
            submitted_at,
            started_at: None,
            finished_at: None,
            progress: None,
            restored: false,
        });
        self.cancels.insert(id, Arc::new(AtomicBool::new(false)));
        (id, self.push_event(event))
    }

    /// Append to the ring (if it keeps anything) and trim it; returns the
    /// event for publication.
    fn push_event(&mut self, event: JobEvent) -> JobEvent {
        if self.event_ring > 0 {
            self.events.push_back(event.clone());
            while self.events.len() > self.event_ring {
                self.events.pop_front();
            }
        }
        event
    }

    fn record_mut(&mut self, id: JobId) -> Option<&mut JobRecord> {
        self.records.iter_mut().find(|r| r.id == id)
    }

    fn mark_started(&mut self, id: JobId) -> Option<JobEvent> {
        let at = Self::now();
        let record = self.record_mut(id)?;
        if record.state.is_terminal() {
            return None;
        }
        record.state = JobState::Running;
        record.started_at = Some(at.clone());
        let seq = self.next_seq();
        Some(self.push_event(JobEvent {
            seq,
            at,
            job: id,
            detail: JobEventDetail::Started,
        }))
    }

    fn set_progress(
        &mut self,
        id: JobId,
        done: u64,
        total: u64,
        message: &str,
    ) -> Option<JobEvent> {
        let at = Self::now();
        let record = self.record_mut(id)?;
        if record.state.is_terminal() {
            return None;
        }
        record.progress = Some(JobProgress {
            done,
            total,
            message: message.to_string(),
        });
        let seq = self.next_seq();
        Some(self.push_event(JobEvent {
            seq,
            at,
            job: id,
            detail: JobEventDetail::Progress {
                done,
                total,
                message: message.to_string(),
            },
        }))
    }

    /// Move a job to its terminal state. On the first (winning) terminal
    /// transition this also returns the ledger entry to persist — the
    /// caller records it in the finished-jobs table so Activity survives
    /// restarts (migration 16). A raced second transition returns `None`
    /// and writes nothing.
    fn finish(
        &mut self,
        id: JobId,
        state: JobState,
        detail: JobEventDetail,
    ) -> Option<(JobEvent, JobLedgerEntry)> {
        let at = Self::now();
        let (kind, submitted_at, state) = {
            let record = self.record_mut(id)?;
            if record.state.is_terminal() {
                // A cancel raced the job's own finish: the first terminal
                // state wins, and no second terminal event is emitted.
                return None;
            }
            record.state = state;
            record.finished_at = Some(at.clone());
            (
                record.kind.clone(),
                record.submitted_at.clone(),
                record.state.clone(),
            )
        };
        self.cancels.remove(&id);
        self.finished_order.push(id);
        while self.finished_order.len() > self.finished_keep {
            let evict = self.finished_order.remove(0);
            self.records.retain(|r| r.id != evict);
        }
        let entry = JobLedgerEntry {
            job_id: id.0,
            kind,
            submitted_at,
            finished_at: at.clone(),
            state: ledger_state_token(&state).to_string(),
            summary: job_result_summary(&state),
            payload: serde_json::to_string(&state).unwrap_or_default(),
        };
        let seq = self.next_seq();
        let event = self.push_event(JobEvent {
            seq,
            at,
            job: id,
            detail,
        });
        Some((event, entry))
    }

    /// Live records plus the restored history, live first — the snapshot
    /// clients reconcile from. Ids already order the merge correctly
    /// (every live id sits above every restored id).
    fn snapshot(&self) -> Vec<JobRecord> {
        let mut jobs = self.records.clone();
        jobs.extend(self.restored.iter().cloned());
        jobs
    }

    fn events_since(&self, cursor: u64) -> JobEventsPage {
        let events = self
            .events
            .iter()
            .filter(|e| e.seq > cursor)
            .cloned()
            .collect();
        let latest_seq = self.next_seq.saturating_sub(1);
        let oldest_kept = self.events.front().map(|e| e.seq).unwrap_or(self.next_seq);
        JobEventsPage {
            events,
            latest_seq,
            oldest_kept,
            trimmed: cursor + 1 < oldest_kept,
            jobs: self.snapshot(),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands crossing the bounded queues
// ---------------------------------------------------------------------------

type WriteOp = Box<dyn FnOnce(&Storage) + Send + 'static>;
type JobOp = Box<dyn FnOnce(&JobContext) -> std::result::Result<Value, JobError> + Send + 'static>;

enum WriteCommand {
    Write(WriteOp),
    Shutdown,
}

enum JobCommand {
    Run { id: JobId, op: JobOp },
    Shutdown,
}

// Set while the writer thread executes a submitted operation, so a
// re-entrant `submit_write` from inside a write fails fast instead of
// deadlocking on itself.
thread_local! {
    static IN_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// ---------------------------------------------------------------------------
// The pool
// ---------------------------------------------------------------------------

/// The database pool: one writer worker thread + two read connections +
/// the background job runner (see the [module documentation](self)).
///
/// Shared as `Arc<DbPool>` (the app, the folder watcher, and job bodies
/// each hold one). Dropping the last handle asks the threads to stop and
/// detaches them; [`DbPool::shutdown`] joins them for deterministic
/// teardown (tests).
pub struct DbPool {
    readers: [Mutex<Storage>; 2],
    write_tx: SyncSender<WriteCommand>,
    job_tx: SyncSender<JobCommand>,
    jobs: Mutex<JobRegistry>,
    on_event: Box<dyn Fn(&JobEvent) + Send + Sync>,
    // Interior-mutable so `spawn` can graft the join handles after the
    // threads (which hold their own Arc clones of this very pool) start.
    writer: Mutex<Option<JoinHandle<()>>>,
    runner: Mutex<Option<JoinHandle<()>>>,
}

impl DbPool {
    /// Open the vault file (creating/migrating as [`Storage::open`] does)
    /// and start the writer, the two read connections, and the job runner.
    ///
    /// The writer's connection runs any pending migration; the read
    /// connections then open the already-current file (per-connection
    /// initialization: pragmas + custom functions on every connection).
    /// `on_event` is invoked for every job event after the registry
    /// updates (never under its lock).
    pub fn spawn(
        path: impl AsRef<std::path::Path>,
        config: PoolConfig,
        on_event: impl Fn(&JobEvent) + Send + Sync + 'static,
    ) -> Result<Arc<Self>> {
        // Writer first: it owns migration duty. Sequential opens, so a
        // pending migration is applied exactly once before readers see the
        // file.
        let writer_storage = Storage::open(path.as_ref())?;
        // Finished-jobs ledger (migration 16): the previous sessions'
        // terminal jobs become read-only history in the registry, loaded
        // here — before the threads start, so no live job can be submitted
        // first and the restored ids stay below every id this session
        // allocates.
        let restored: Vec<JobRecord> = writer_storage
            .job_ledger_recent(JOB_LEDGER_KEEP as u32)?
            .iter()
            .enumerate()
            .map(|(offset, row)| restored_record(row, offset as u64))
            .collect();
        let reader_a = Storage::open(path.as_ref())?;
        let reader_b = Storage::open(path.as_ref())?;
        let (write_tx, write_rx) =
            std::sync::mpsc::sync_channel::<WriteCommand>(config.write_queue_capacity.max(1));
        let (job_tx, job_rx) =
            std::sync::mpsc::sync_channel::<JobCommand>(config.job_queue_capacity.max(1));
        let pool = Arc::new(Self {
            readers: [Mutex::new(reader_a), Mutex::new(reader_b)],
            write_tx,
            job_tx,
            jobs: Mutex::new(JobRegistry::with_restored(&config, restored)),
            on_event: Box::new(on_event),
            writer: Mutex::new(None),
            runner: Mutex::new(None),
        });
        let writer = std::thread::Builder::new()
            .name("vault-writer".into())
            .spawn({
                let pool = Arc::clone(&pool);
                let storage = writer_storage;
                move || run_writer(&pool, storage, write_rx)
            })
            .map_err(spawn_error)?;
        let runner = std::thread::Builder::new()
            .name("vault-jobs".into())
            .spawn({
                let pool = Arc::clone(&pool);
                move || run_jobs(&pool, job_rx)
            })
            .map_err(spawn_error)?;
        *pool
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(writer);
        *pool
            .runner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(runner);
        Ok(pool)
    }

    // -- Reads ------------------------------------------------------------

    /// Lock one of the two read connections, preferring an idle slot.
    fn read_lock(&self) -> MutexGuard<'_, Storage> {
        if let Ok(guard) = self.readers[0].try_lock() {
            return guard;
        }
        if let Ok(guard) = self.readers[1].try_lock() {
            return guard;
        }
        // Both busy: wait for slot 0 (poisoning cannot kill reads here —
        // a panicked reader leaves a working SQLite connection behind).
        self.readers[0]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run a read-only operation on an idle read connection. Reads never
    /// queue behind the writer (WAL): this returns while writes commit.
    pub fn with_read<R>(&self, op: impl FnOnce(&Storage) -> R) -> R {
        let guard = self.read_lock();
        op(&guard)
    }

    // -- Writes -----------------------------------------------------------

    /// Submit a write operation to the bounded queue and wait for it.
    ///
    /// The queue's capacity bounds outstanding writes (backpressure);
    /// when full this blocks until the writer drains — callers on UI-ish
    /// paths should already be off the UI thread (the Tauri commands are).
    /// Returns the operation's result; an error means the write itself
    /// failed or the writer stopped. Re-entrant submission from inside a
    /// write is refused (it would deadlock on the single writer).
    pub fn submit_write<R: Send + 'static>(
        &self,
        op: impl FnOnce(&Storage) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        if IN_WRITE.with(|flag| flag.get()) {
            return Err(Error::Jobs(
                "re-entrant write: a write operation must not submit another write".into(),
            ));
        }
        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<Result<R>>();
        let boxed: WriteOp = Box::new(move |storage| {
            let out = op(storage);
            let _ = ack_tx.send(out);
        });
        self.write_tx
            .send(WriteCommand::Write(boxed))
            .map_err(|_| writer_stopped())?;
        ack_rx.recv().map_err(|_| writer_stopped())?
    }

    // -- Jobs -------------------------------------------------------------

    /// Submit a background job; returns its [`JobId`] immediately (the
    /// job runs on the runner thread). Blocks only when the bounded job
    /// queue is full (backpressure).
    pub fn submit_job(
        &self,
        kind: impl Into<String>,
        op: impl FnOnce(&JobContext) -> std::result::Result<Value, JobError> + Send + 'static,
    ) -> std::result::Result<JobId, JobError> {
        let (id, event) = self.with_jobs_mut(|reg| reg.register(kind.into()));
        let cmd = JobCommand::Run {
            id,
            op: Box::new(op),
        };
        match self.job_tx.send(cmd) {
            Ok(()) => {
                (self.on_event)(&event);
                Ok(id)
            }
            Err(_) => Err(self.fail_submission(id, runner_stopped())),
        }
    }

    /// [`DbPool::submit_job`] that never waits: a full queue is a typed
    /// [`JobError::QueueFull`] instead of backpressure.
    pub fn try_submit_job(
        &self,
        kind: impl Into<String>,
        op: impl FnOnce(&JobContext) -> std::result::Result<Value, JobError> + Send + 'static,
    ) -> std::result::Result<JobId, JobError> {
        let (id, event) = self.with_jobs_mut(|reg| reg.register(kind.into()));
        let cmd = JobCommand::Run {
            id,
            op: Box::new(op),
        };
        match self.job_tx.try_send(cmd) {
            Ok(()) => {
                (self.on_event)(&event);
                Ok(id)
            }
            Err(TrySendError::Full(_)) => Err(self.fail_submission(
                id,
                JobError::QueueFull {
                    queue: "job".into(),
                },
            )),
            Err(TrySendError::Disconnected(_)) => Err(self.fail_submission(id, runner_stopped())),
        }
    }

    /// A submission that never reached the runner fails terminally, so no
    /// client waits on a queued job that cannot start.
    fn fail_submission(&self, id: JobId, error: JobError) -> JobError {
        let error = error.clone();
        if let Some((event, entry)) = self.with_jobs_mut(|reg| {
            let detail_error = error.clone();
            reg.finish(
                id,
                JobState::Failed {
                    error: error.clone(),
                },
                JobEventDetail::Failed {
                    error: detail_error,
                },
            )
        }) {
            persist_ledger_entry(self, entry);
            (self.on_event)(&event);
        }
        error
    }

    /// Ask a job to stop. Cooperative: the job observes it via
    /// [`JobContext::is_cancelled`] and returns `Err(JobError::Cancelled)`.
    /// Returns false when the id is unknown or already terminal.
    pub fn cancel_job(&self, id: JobId) -> bool {
        self.with_jobs_mut(|reg| {
            let Some(record) = reg.records.iter().find(|r| r.id == id) else {
                return false;
            };
            if record.state.is_terminal() {
                return false;
            }
            if let Some(flag) = reg.cancels.get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
            true
        })
    }

    /// One job's current record (active or recently finished).
    pub fn job(&self, id: JobId) -> Option<JobRecord> {
        self.with_jobs_mut(|reg| reg.records.iter().find(|r| r.id == id).cloned())
    }

    /// All current records: active jobs plus recently finished ones, plus
    /// the restored history a restart inherited.
    pub fn jobs(&self) -> Vec<JobRecord> {
        self.with_jobs_mut(|reg| reg.snapshot())
    }

    /// The polling-recovery page for a cursor (see the
    /// [module documentation](self)); `events_since(0)` fetches everything
    /// retained.
    pub fn events_since(&self, cursor: u64) -> JobEventsPage {
        self.with_jobs_mut(|reg| reg.events_since(cursor))
    }

    fn with_jobs_mut<R>(&self, f: impl FnOnce(&mut JobRegistry) -> R) -> R {
        let mut reg = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut reg)
    }

    /// Stop the threads and join them (deterministic teardown for tests).
    pub fn shutdown(self: &Arc<Self>) {
        let _ = self.job_tx.send(JobCommand::Shutdown);
        let _ = self.write_tx.send(WriteCommand::Shutdown);
        // The threads drop their Arc clones when their loops return;
        // joining from here is safe because the shutdown commands are
        // already queued ahead of any pending work.
        let runner = self
            .runner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = runner {
            let _ = handle.join();
        }
        if let Some(handle) = writer {
            let _ = handle.join();
        }
    }
}

impl Drop for DbPool {
    fn drop(&mut self) {
        // Ask both threads to stop, then detach them: teardown must not
        // block on an in-flight job (mirrors FolderWatcher's stance).
        let _ = self.job_tx.send(JobCommand::Shutdown);
        let _ = self.write_tx.send(WriteCommand::Shutdown);
        let take = |slot: &Mutex<Option<JoinHandle<()>>>| {
            slot.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        };
        let threads: Vec<JoinHandle<()>> = [take(&self.writer), take(&self.runner)]
            .into_iter()
            .flatten()
            .collect();
        if !threads.is_empty() {
            std::thread::spawn(move || {
                for handle in threads {
                    let _ = handle.join();
                }
            });
        }
    }
}

/// The writer loop: executes queued writes strictly one at a time.
fn run_writer(_pool: &DbPool, storage: Storage, rx: Receiver<WriteCommand>) {
    for cmd in rx {
        match cmd {
            WriteCommand::Write(op) => {
                IN_WRITE.with(|flag| flag.set(true));
                // A panicking write must not kill the writer (the failed
                // op's ack channel drops, and its caller sees the
                // writer-stop error instead).
                let _ = catch_unwind(AssertUnwindSafe(|| op(&storage)));
                IN_WRITE.with(|flag| flag.set(false));
            }
            WriteCommand::Shutdown => break,
        }
    }
}

/// The job loop: runs submitted jobs one at a time, recording typed state
/// transitions and events.
fn run_jobs(pool: &Arc<DbPool>, rx: Receiver<JobCommand>) {
    for cmd in rx {
        match cmd {
            JobCommand::Run { id, op } => run_job(pool, id, op),
            JobCommand::Shutdown => break,
        }
    }
}

fn run_job(pool: &Arc<DbPool>, id: JobId, op: JobOp) {
    if let Some(event) = pool.with_jobs_mut(|reg| reg.mark_started(id)) {
        (pool.on_event)(&event);
    }
    let Some(cancel) = pool.with_jobs_mut(|reg| reg.cancels.get(&id).cloned()) else {
        return; // terminal before it started (cancelled in queue + raced)
    };
    if cancel.load(Ordering::Relaxed) {
        finish_job(pool, id, JobState::Cancelled, JobEventDetail::Cancelled);
        return;
    }
    let ctx = JobContext {
        pool,
        cancel: &cancel,
        id,
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| op(&ctx)));
    match outcome {
        Ok(Ok(result)) => finish_job(
            pool,
            id,
            JobState::Succeeded {
                result: result.clone(),
            },
            JobEventDetail::Succeeded { result },
        ),
        Ok(Err(JobError::Cancelled)) => {
            finish_job(pool, id, JobState::Cancelled, JobEventDetail::Cancelled)
        }
        Ok(Err(error)) => finish_job(
            pool,
            id,
            JobState::Failed {
                error: error.clone(),
            },
            JobEventDetail::Failed { error },
        ),
        Err(_) => {
            let error = JobError::Internal {
                message: "job panicked".into(),
            };
            finish_job(
                pool,
                id,
                JobState::Failed {
                    error: error.clone(),
                },
                JobEventDetail::Failed { error },
            );
        }
    }
}

fn finish_job(pool: &DbPool, id: JobId, state: JobState, detail: JobEventDetail) {
    if let Some((event, entry)) = pool.with_jobs_mut(|reg| reg.finish(id, state, detail)) {
        persist_ledger_entry(pool, entry);
        (pool.on_event)(&event);
    }
}

fn spawn_error(e: std::io::Error) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Finished-jobs ledger (migration 16)
// ---------------------------------------------------------------------------

/// The ledger's state token for a terminal state (`"succeeded"`,
/// `"failed"`, `"cancelled"`); non-terminal states never reach the ledger.
fn ledger_state_token(state: &JobState) -> &'static str {
    match state {
        JobState::Succeeded { .. } => "succeeded",
        JobState::Failed { .. } => "failed",
        JobState::Cancelled => "cancelled",
        JobState::Queued | JobState::Running => "running",
    }
}

/// The one-line result summary stored beside a finished job — the same
/// result part the UI's `describeJobState` renders after "done" for
/// succeeded jobs ("5,505 new, 3 present", "8 embedded"), the error
/// message for failed ones, empty for cancelled. Mirrors
/// `describeResult` in src/jobsview.ts; keep the two in step.
fn job_result_summary(state: &JobState) -> String {
    match state {
        JobState::Succeeded { result } => {
            let Some(object) = result.as_object() else {
                return String::new();
            };
            // Imports that run a media pass return { summary, media } —
            // the ingest counts live one level down; flat results (embed
            // jobs) read directly.
            let counts = object
                .get("summary")
                .and_then(|s| s.as_object())
                .unwrap_or(object);
            let mut bits: Vec<String> = Vec::new();
            if let Some(inserted) = counts.get("inserted").and_then(Value::as_u64) {
                bits.push(format!("{inserted} new"));
                if let Some(skipped) = counts.get("skipped").and_then(Value::as_u64) {
                    if skipped > 0 {
                        bits.push(format!("{skipped} present"));
                    }
                }
            }
            if let Some(embedded) = object.get("embedded").and_then(Value::as_u64) {
                if embedded > 0 {
                    bits.push(format!("{embedded} embedded"));
                }
            }
            bits.join(", ")
        }
        JobState::Failed { error } => error.to_string(),
        JobState::Cancelled | JobState::Queued | JobState::Running => String::new(),
    }
}

/// Map one ledger row back to the read-only record the panel renders
/// (newest-first rows get descending synthetic ids from
/// [`RESTORED_JOB_ID_BASE`], so they sort below every live id). The
/// stored payload is the serialized terminal state; if it cannot be
/// parsed (a future format change, say), the row degrades to the stored
/// summary instead of vanishing.
fn restored_record(row: &JobLedgerRow, offset: u64) -> JobRecord {
    let state = serde_json::from_str::<JobState>(&row.payload).unwrap_or_else(|_| {
        match row.state.as_str() {
            "failed" => JobState::Failed {
                error: JobError::Internal {
                    message: row.summary.clone(),
                },
            },
            "cancelled" => JobState::Cancelled,
            _ => JobState::Succeeded {
                result: Value::String(row.summary.clone()),
            },
        }
    });
    JobRecord {
        id: JobId(RESTORED_JOB_ID_BASE - offset),
        kind: row.kind.clone(),
        state,
        submitted_at: row.submitted_at.clone(),
        started_at: None,
        finished_at: Some(row.finished_at.clone()),
        progress: None,
        restored: true,
    }
}

/// Best-effort ledger append: the write goes through the bounded queue
/// like every other write (never under the registry lock). A ledger
/// failure must never mask the job's own terminal event — it is dropped
/// on the floor, and the in-session panel is unaffected either way.
fn persist_ledger_entry(pool: &DbPool, entry: JobLedgerEntry) {
    let _ = pool.submit_write(move |s| s.record_job_ledger(&entry));
}

fn writer_stopped() -> Error {
    Error::Jobs("the write operation panicked or the writer thread stopped".into())
}

fn runner_stopped() -> JobError {
    JobError::Internal {
        message: "job runner is not running".into(),
    }
}

// ---------------------------------------------------------------------------
// Job execution context
// ---------------------------------------------------------------------------

/// The handle a job body runs with. Storage access is marshaled through
/// the pool (`read`/`write`); network waits, parsing, and CPU work happen
/// on the runner thread with no storage held, which is what keeps network
/// operations out of the writer and out of every transaction.
pub struct JobContext<'a> {
    pool: &'a DbPool,
    cancel: &'a AtomicBool,
    id: JobId,
}

impl<'a> JobContext<'a> {
    /// The job's own id.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// The pool the job runs against (e.g. for [`SyncStorage`] adapters
    /// like [`PoolSync`]).
    pub fn pool(&self) -> &'a DbPool {
        self.pool
    }

    /// Run a read-only operation on an idle read connection.
    pub fn read<R>(&self, op: impl FnOnce(&Storage) -> R) -> R {
        self.pool.with_read(op)
    }

    /// Submit a write through the bounded queue and wait for it.
    pub fn write<R: Send + 'static>(
        &self,
        op: impl FnOnce(&Storage) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        self.pool.submit_write(op)
    }

    /// Report progress (also visible through the event stream).
    pub fn progress(&self, done: u64, total: u64, message: &str) {
        if let Some(event) = self
            .pool
            .with_jobs_mut(|reg| reg.set_progress(self.id, done, total, message))
        {
            (self.pool.on_event)(&event);
        }
    }

    /// Whether cancellation was requested; check at safe points.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Bounded ingest over the pool (M2.2)
// ---------------------------------------------------------------------------

/// [`IngestSink`] over the pool: every checkpoint read and batch commit
/// round-trips the bounded write queue, so a bounded import's parsing and
/// media copying (which happen on the caller's thread between commits)
/// never occupy the writer or a read connection while they read or copy.
pub struct PoolSink<'a>(pub &'a DbPool);

impl IngestSink for PoolSink<'_> {
    fn checkpoint(&mut self, import_id: &str) -> Result<Option<u64>> {
        let import_id = import_id.to_string();
        self.0
            .submit_write(move |s| s.import_checkpoint(&import_id))
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
    ) -> Result<ImportBatchCounts> {
        // The write queue is 'static, so the borrowed call frame is cloned
        // into the closure — one bounded batch's worth of data.
        let import_id = import_id.to_string();
        let source = source.to_string();
        let messages = messages.to_vec();
        let issues: Vec<(u64, String, String)> = issues
            .iter()
            .map(|i| (i.position, i.external_ref.to_string(), i.reason.to_string()))
            .collect();
        let media_objects: Vec<crate::media::UnlinkedObject> = media_objects.to_vec();
        self.0.submit_write(move |s| {
            let records: Vec<ImportIssueRecord<'_>> = issues
                .iter()
                .map(|(position, external_ref, reason)| ImportIssueRecord {
                    position: *position,
                    external_ref: external_ref.as_str(),
                    reason: reason.as_str(),
                })
                .collect();
            s.commit_import_batch(
                &import_id,
                &source,
                &messages,
                &records,
                &media_objects,
                committed_after,
                completes,
            )
        })
    }

    fn linked_media_digests(&mut self) -> Result<std::collections::HashSet<String>> {
        self.0.with_read(|s| s.message_media_digests())
    }
}

// ---------------------------------------------------------------------------
// IMAP sync over the pool
// ---------------------------------------------------------------------------

/// The owned mirror of one [`MailBatchItem`] — the queue is `'static`, so
/// [`PoolSync::commit_batch`] clones the borrowed call frame into the
/// write closure and re-borrows it on the writer lane.
struct OwnedBatchMail {
    message: NewMessage,
    subject: String,
    namespace: String,
    external_key: String,
    revision_hash: String,
    scope: String,
    provider_item_id: String,
    occurrence_uidvalidity: Option<u32>,
}

impl From<&MailBatchItem<'_>> for OwnedBatchMail {
    fn from(item: &MailBatchItem<'_>) -> Self {
        Self {
            message: item.message.clone(),
            subject: item.subject.to_string(),
            namespace: item.identity.namespace.to_string(),
            external_key: item.identity.external_key.to_string(),
            revision_hash: item.identity.revision_hash.to_string(),
            scope: item.occurrence.scope.to_string(),
            provider_item_id: item.occurrence.provider_item_id.to_string(),
            occurrence_uidvalidity: item.occurrence.uidvalidity,
        }
    }
}

/// [`SyncStorage`] adapter that marshals every storage call of an IMAP
/// sync through the pool's bounded write queue.
///
/// The sync itself runs on the job runner thread: between two storage
/// calls it talks to the IMAP server, so a stalled network read occupies
/// neither the writer nor a read connection — the M2.1 contract
/// ("no network inside the write lane / storage lock / transaction")
/// holds by construction, batch by batch.
pub struct PoolSync<'a>(pub &'a DbPool);

impl SyncStorage for PoolSync<'_> {
    fn ensure_instance(&self, config: &ImapConfig) -> Result<String> {
        let label = config.display_label();
        let key = config.connector_key();
        let config_json = config.config_json();
        self.0.submit_write(move |s| {
            s.ensure_source_instance(&NewSourceInstance {
                kind: "imap",
                label: &label,
                connector_key: &key,
                config_json: &config_json,
            })
        })
    }

    fn record_attempt(&self, instance: &str) -> Result<()> {
        let instance = instance.to_string();
        self.0
            .submit_write(move |s| s.record_source_attempt(&instance))
    }

    fn record_success(&self, instance: &str) -> Result<()> {
        let instance = instance.to_string();
        self.0
            .submit_write(move |s| s.record_source_success(&instance))
    }

    fn record_failure(&self, instance: &str, failure: &SyncFailure, message: &str) -> Result<()> {
        let instance = instance.to_string();
        let class = failure.as_str().to_string();
        let message = message.to_string();
        self.0
            .submit_write(move |s| s.record_source_failure(&instance, &class, &message))
    }

    fn checkpoint(
        &self,
        instance: &str,
        folder: &str,
    ) -> Result<Option<crate::message::ImapSyncState>> {
        let instance = instance.to_string();
        let folder = folder.to_string();
        self.0
            .submit_write(move |s| s.get_imap_checkpoint(&instance, &folder))
    }

    fn commit_batch(
        &self,
        instance: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
        mails: &[MailBatchItem<'_>],
        failures: &[BatchFailure<'_>],
        resolved: &[u32],
    ) -> Result<MailBatchOutcome> {
        // One write round trip carries the whole batch: the network waits
        // that produced it happened on this thread, before the submission,
        // and the transaction on the writer lane never spans them.
        let instance = instance.to_string();
        let folder = folder.to_string();
        let mails: Vec<OwnedBatchMail> = mails.iter().map(OwnedBatchMail::from).collect();
        let failures: Vec<(u32, String)> = failures
            .iter()
            .map(|f| (f.uid, f.reason.to_string()))
            .collect();
        let resolved = resolved.to_vec();
        self.0.submit_write(move |s| {
            let items: Vec<MailBatchItem<'_>> = mails
                .iter()
                .map(|m| MailBatchItem {
                    message: &m.message,
                    subject: &m.subject,
                    identity: ConnectorIdentity {
                        namespace: &m.namespace,
                        external_key: &m.external_key,
                        revision_hash: &m.revision_hash,
                    },
                    occurrence: Occurrence {
                        scope: &m.scope,
                        provider_item_id: &m.provider_item_id,
                        uidvalidity: m.occurrence_uidvalidity,
                    },
                })
                .collect();
            let batch_failures: Vec<BatchFailure<'_>> = failures
                .iter()
                .map(|(uid, reason)| BatchFailure { uid: *uid, reason })
                .collect();
            s.commit_imap_batch(
                &instance,
                &folder,
                uidvalidity,
                last_uid,
                &items,
                &batch_failures,
                &resolved,
            )
        })
    }

    fn failed_uids(&self, instance: &str, folder: &str, uidvalidity: u32) -> Result<Vec<u32>> {
        let instance = instance.to_string();
        let folder = folder.to_string();
        self.0
            .submit_write(move |s| s.imap_failed_uids(&instance, &folder, uidvalidity))
    }
}
