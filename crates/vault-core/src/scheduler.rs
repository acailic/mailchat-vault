//! The periodic sync scheduler (M2.4): job-backed rounds over every
//! configured IMAP source, on a user-opt-in interval.
//!
//! Where the M2.3 coordinator owns *one* round ("sync everything now"),
//! the scheduler owns *when* rounds happen without the user pressing
//! anything: opted in, it submits one coordinator round as an ordinary
//! pool job ([`DbPool::try_submit_job`], kind
//! [`SCHEDULED_JOB_KIND`] = `imap_sync_all`) every interval — default
//! 15 minutes — so scheduled rounds are visible, cancellable, and
//! serialized by the same v2 job protocol the UI already renders.
//!
//! # Scheduling contract (the invariants this module is built on)
//!
//! - **Opt-in.** The persisted [`SyncSchedule`] starts disabled; nothing
//!   runs periodically until the user enables it. The schedule lives in
//!   the vault database (a singleton row, migration 11) — the same
//!   durable home watched folders have — and re-arms on the next start.
//! - **Never overlaps, never queues missed runs.** There is exactly one
//!   deadline and at most one scheduled round queued or running. A new
//!   round is submitted only after the previous round's job reached a
//!   terminal state; while one is active, a passing deadline is simply
//!   observed, not acted on. A round that outlasts the interval delays
//!   the next round — it never stacks a second one behind it.
//! - **Sleep/resume/offline collapses to one catch-up round.** The next
//!   deadline is always `last submission + interval` — never a wall-clock
//!   schedule grid — so hours of suspended process, a sleeping machine,
//!   or a network outage produce at most one pending round on wake, then
//!   the cadence resumes. There is no timer wheel to discharge: missed
//!   time cannot become a burst of submissions by construction.
//! - **Job-backed.** The round body is injected (`run_round`): the Tauri
//!   shell passes the exact M2.3 coordinator closure the manual
//!   `imap_sync_all` job runs; tests inject a recording mock. The
//!   scheduler never touches the network itself.
//!
//! The loop mirrors [`crate::watch::FolderWatcher`]: a control channel
//! (enable/disable/reconfigure with an ack), a shared status snapshot
//! published on every change, a cooperative shutdown, and a `tick`-paced
//! evaluation of the single deadline. Time is read through an injectable
//! clock so the regression suite can jump it.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::jobs::{DbPool, JobContext, JobError, JobId};

/// The job kind scheduled rounds are submitted as — the same kind the
/// manual coordinator round uses, so the jobs panel labels both
/// identically.
pub const SCHEDULED_JOB_KIND: &str = "imap_sync_all";

/// Default interval between scheduled rounds (15 minutes).
pub const DEFAULT_INTERVAL_MS: u64 = 15 * 60 * 1000;
/// Shortest accepted interval (a shorter period would hammer servers).
pub const MIN_INTERVAL_MS: u64 = 60 * 1000;
/// Longest accepted interval (one day).
pub const MAX_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

/// The persisted schedule: whether periodic sync is on, and its interval.
///
/// Stored as one singleton row in the vault database (see
/// [`crate::storage::Storage::set_sync_schedule`]); the interval is always kept inside
/// `MIN_INTERVAL_MS..=MAX_INTERVAL_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSchedule {
    pub enabled: bool,
    /// Period between scheduled rounds, in milliseconds. Millisecond
    /// granularity keeps the regression suite honest (real-time
    /// sub-second cadences); the UI speaks minutes.
    pub interval_ms: u64,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_ms: DEFAULT_INTERVAL_MS,
        }
    }
}

impl SyncSchedule {
    /// This schedule with the interval clamped into the accepted range
    /// (the product bounds; see [`MIN_INTERVAL_MS`]).
    pub fn normalized(self) -> Self {
        self.normalized_within(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
    }

    /// This schedule with the interval clamped into `min_ms..=max_ms`.
    /// The scheduler clamps with its configured bounds before persisting
    /// (the app runs the product bounds; tests shrink them to exercise
    /// fast cadences in real time).
    pub fn normalized_within(self, min_ms: u64, max_ms: u64) -> Self {
        Self {
            enabled: self.enabled,
            interval_ms: self.interval_ms.clamp(min_ms, max_ms),
        }
    }
}

/// The scheduler's published status, refreshed on every change (enable,
/// disable, reconfigure, round submitted, round finished).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncSchedulerStatus {
    /// Whether the loop thread is alive.
    pub running: bool,
    /// The persisted schedule (what `sync_schedule_get` also returns).
    pub enabled: bool,
    /// The configured interval, in milliseconds.
    pub interval_ms: u64,
    /// The scheduled round currently queued or running (`None` when the
    /// previous round already finished and none is pending).
    pub active_job: Option<u64>,
    /// Whole seconds until the next scheduled round, rounded up
    /// (`None` while disabled). Approximate by design: it is refreshed
    /// when the status is published, not every second.
    pub next_due_in_secs: Option<u64>,
    /// Scheduled rounds submitted since this scheduler started.
    pub rounds_submitted: u64,
}

/// The round body the scheduler submits as a job. The app injects the
/// M2.3 coordinator round (credential resolution from the OS keyring
/// included); tests inject a mock that records (or stalls) invocations.
pub type RoundFn = Arc<dyn Fn(&JobContext) -> Result<serde_json::Value, JobError> + Send + Sync>;

/// A time source for the scheduler loop. Injectable so tests can jump
/// the clock (simulating suspend/offline gaps); the default reads the
/// real monotonic clock.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Tuning knobs of the scheduler loop. Tests use a short `tick` and an
/// injectable clock.
#[derive(Clone)]
pub struct SchedulerConfig {
    /// How often the loop wakes to evaluate the deadline and job state.
    /// A round may fire up to one tick late; the default is noise next
    /// to the 15-minute cadence.
    pub tick: Duration,
    /// The clock deadlines are computed against.
    pub clock: Clock,
    /// Interval bounds applied when a schedule is set or loaded
    /// (defaults: [`MIN_INTERVAL_MS`]..=[`MAX_INTERVAL_MS`]).
    pub min_interval_ms: u64,
    pub max_interval_ms: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(1),
            clock: Arc::new(Instant::now),
            min_interval_ms: MIN_INTERVAL_MS,
            max_interval_ms: MAX_INTERVAL_MS,
        }
    }
}

/// A manually advanced clock for the regression suite: `advance` jumps
/// time without sleeping, which is how suspend/offline gaps are
/// simulated (the loop still ticks in real time).
#[derive(Clone, Debug)]
pub struct ManualClock {
    now: Arc<Mutex<Instant>>,
}

impl ManualClock {
    pub fn new(start: Instant) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Jump the clock forward by `by` (monotonic: only forward).
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|p| p.into_inner());
        *now += by;
    }

    /// The injectable [`Clock`] view of this manual clock.
    pub fn clock(&self) -> Clock {
        let now = Arc::clone(&self.now);
        Arc::new(move || *now.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

enum Command {
    /// Apply a new schedule: the loop clamps it to its configured
    /// bounds, persists it, re-arms, and acks with the new status.
    Set {
        schedule: SyncSchedule,
        ack: Sender<Result<SyncSchedulerStatus, String>>,
    },
    Shutdown,
}

/// The opt-in periodic-sync scheduler (see the [module
/// documentation](self) for the scheduling contract).
///
/// Shared as `Arc<SyncScheduler>` by the app; dropping the last handle
/// asks the loop to stop and detaches it ([`SyncScheduler::shutdown`]
/// joins, for tests).
pub struct SyncScheduler {
    control: Sender<Command>,
    status: Arc<Mutex<SyncSchedulerStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl SyncScheduler {
    /// Start the scheduler loop over `pool`, arming from the schedule
    /// persisted in the vault (disabled until someone opts in). Every
    /// status change invokes `on_change` with the fresh snapshot.
    pub fn spawn(
        pool: Arc<DbPool>,
        config: SchedulerConfig,
        run_round: RoundFn,
        on_change: impl Fn(&SyncSchedulerStatus) + Send + Sync + 'static,
    ) -> Self {
        let (control_tx, control_rx) = std::sync::mpsc::channel::<Command>();
        let status = Arc::new(Mutex::new(SyncSchedulerStatus {
            running: false,
            enabled: false,
            interval_ms: DEFAULT_INTERVAL_MS,
            active_job: None,
            next_due_in_secs: None,
            rounds_submitted: 0,
        }));
        let status_for_thread = Arc::clone(&status);
        let thread = std::thread::Builder::new()
            .name("sync-scheduler".into())
            .spawn(move || {
                run_loop(
                    pool,
                    config,
                    run_round,
                    Box::new(on_change),
                    control_rx,
                    status_for_thread,
                )
            })
            .ok();
        if thread.is_none() {
            // Thread could not start (resource exhaustion): record it in
            // the shared status instead of panicking.
            lock_status(&status).running = false;
        }
        Self {
            control: control_tx,
            status,
            thread,
        }
    }

    /// Apply a new schedule: persisted by the loop (single writer)
    /// clamped to the loop's configured interval bounds, and armed.
    /// Disabling cancels a still-active scheduled round (cooperatively —
    /// it stops at its next safe point).
    pub fn set(&self, schedule: SyncSchedule) -> Result<SyncSchedulerStatus, String> {
        let (ack, ack_rx) = std::sync::mpsc::channel();
        self.control
            .send(Command::Set { schedule, ack })
            .map_err(|_| "scheduler loop is not running".to_string())?;
        ack_rx
            .recv()
            .map_err(|_| "scheduler loop stopped while applying the schedule".to_string())?
    }

    /// The current status snapshot.
    pub fn status(&self) -> SyncSchedulerStatus {
        lock_status(&self.status).clone()
    }

    /// Stop the loop and wait for it (tests). Ordinary drops send
    /// `Shutdown` without joining, so app teardown never blocks on a
    /// round that is mid-sync.
    pub fn shutdown(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Command::Shutdown);
            let _ = thread.join();
        }
    }
}

impl Drop for SyncScheduler {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Command::Shutdown);
            // Detach: the process is exiting; do not block teardown.
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
    }
}

fn lock_status(status: &Arc<Mutex<SyncSchedulerStatus>>) -> MutexGuard<'_, SyncSchedulerStatus> {
    status.lock().unwrap_or_else(|p| p.into_inner())
}

/// Everything the loop tracks; the snapshot published to the UI is a
/// pure function of this plus the clock.
struct LoopState {
    schedule: SyncSchedule,
    /// The single deadline: submission time + interval. `None` while
    /// disabled.
    deadline: Option<Instant>,
    /// The scheduled round still queued or running.
    active: Option<JobId>,
    rounds_submitted: u64,
    running: bool,
}

impl LoopState {
    fn snapshot(&self, now: Instant) -> SyncSchedulerStatus {
        SyncSchedulerStatus {
            running: self.running,
            enabled: self.schedule.enabled,
            interval_ms: self.schedule.interval_ms,
            active_job: self.active.map(|id| id.0),
            next_due_in_secs: self
                .deadline
                .map(|d| d.saturating_duration_since(now).as_secs_f64().ceil() as u64),
            rounds_submitted: self.rounds_submitted,
        }
    }
}

fn run_loop(
    pool: Arc<DbPool>,
    config: SchedulerConfig,
    run_round: RoundFn,
    on_change: Box<dyn Fn(&SyncSchedulerStatus) + Send + Sync>,
    control: Receiver<Command>,
    shared: Arc<Mutex<SyncSchedulerStatus>>,
) {
    // Arm from the persisted schedule. A read failure leaves the
    // scheduler disabled — the vault itself is in trouble then, and a
    // periodic sync must not paper over it.
    let persisted = pool.with_read(|s| s.sync_schedule()).unwrap_or_default();
    let now = (config.clock)();
    let mut state = LoopState {
        schedule: persisted.normalized_within(config.min_interval_ms, config.max_interval_ms),
        deadline: persisted
            .enabled
            .then(|| now + Duration::from_millis(persisted.interval_ms)),
        active: None,
        rounds_submitted: 0,
        running: true,
    };
    // Publish on transitions only (arm, set, submit, finish, stop) — a
    // per-tick event stream would be pure noise; the countdown in a
    // published snapshot is approximate (`~N min` in the UI) by design.
    publish(&shared, &on_change, state.snapshot(now));

    loop {
        let mut changed = false;
        match control.recv_timeout(config.tick) {
            Ok(Command::Shutdown) => break,
            Ok(Command::Set { schedule, ack }) => {
                changed = apply_set(
                    &pool, &config, &mut state, schedule, &shared, &on_change, &ack,
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        match tick(&pool, &config, &mut state, &run_round) {
            TickOutcome::Stopped => break,
            TickOutcome::Changed => changed = true,
            TickOutcome::Unchanged => {}
        }
        if changed {
            let now = (config.clock)();
            publish(&shared, &on_change, state.snapshot(now));
        }
    }
    state.running = false;
    let now = (config.clock)();
    publish(&shared, &on_change, state.snapshot(now));
}

/// Apply one `Set` command: clamp to the configured bounds, persist the
/// schedule (single writer: this loop), re-arm the deadline from *now*,
/// cancel an active round on disable, then ack and publish. Returns
/// whether the status changed.
fn apply_set(
    pool: &Arc<DbPool>,
    config: &SchedulerConfig,
    state: &mut LoopState,
    schedule: SyncSchedule,
    shared: &Arc<Mutex<SyncSchedulerStatus>>,
    on_change: &(dyn Fn(&SyncSchedulerStatus) + Send + Sync),
    ack: &Sender<Result<SyncSchedulerStatus, String>>,
) -> bool {
    let schedule = schedule.normalized_within(config.min_interval_ms, config.max_interval_ms);
    match pool.submit_write(move |s| s.set_sync_schedule(schedule)) {
        Ok(schedule) => {
            let now = (config.clock)();
            if schedule.enabled {
                // Fresh arm: one full interval from now (enabling is not
                // "sync now" — the manual buttons exist for that).
                state.deadline = Some(now + Duration::from_millis(schedule.interval_ms));
            } else {
                state.deadline = None;
                if let Some(id) = state.active {
                    // Cooperative: the round stops at its next safe point
                    // (between sources, between fetch batches).
                    pool.cancel_job(id);
                }
            }
            state.schedule = schedule;
            let snapshot = state.snapshot(now);
            publish(shared, on_change, snapshot.clone());
            let _ = ack.send(Ok(snapshot));
            true
        }
        Err(e) => {
            let _ = ack.send(Err(e.to_string()));
            false
        }
    }
}

enum TickOutcome {
    Unchanged,
    Changed,
    Stopped,
}

/// One deadline evaluation: observe the active round's end, then submit
/// the next round if due — at most one scheduled round is ever queued or
/// running (see the module's scheduling contract).
fn tick(
    pool: &Arc<DbPool>,
    config: &SchedulerConfig,
    state: &mut LoopState,
    run_round: &RoundFn,
) -> TickOutcome {
    let now = (config.clock)();
    let mut changed = false;

    // Observe the active round's end (the record may also have been
    // evicted from the finished-jobs window — that too means terminal).
    if let Some(id) = state.active {
        let terminal = pool.job(id).map(|r| r.state.is_terminal()).unwrap_or(true);
        if terminal {
            state.active = None;
            changed = true;
        }
    }

    // A passing deadline while a round is active is observed, never
    // acted on: no overlap, no queue of missed rounds.
    if !state.schedule.enabled || state.active.is_some() {
        return if changed {
            TickOutcome::Changed
        } else {
            TickOutcome::Unchanged
        };
    }
    let due = state.deadline.is_some_and(|d| now >= d);
    if !due {
        return if changed {
            TickOutcome::Changed
        } else {
            TickOutcome::Unchanged
        };
    }

    let round = Arc::clone(run_round);
    match pool.try_submit_job(SCHEDULED_JOB_KIND, move |ctx| round(ctx)) {
        Ok(id) => {
            state.active = Some(id);
            state.rounds_submitted += 1;
            // The next deadline always anchors at the actual submission
            // — never a schedule grid — so missed time collapses into
            // this one catch-up round instead of queueing.
            let now = (config.clock)();
            state.deadline = Some(now + Duration::from_millis(state.schedule.interval_ms));
            TickOutcome::Changed
        }
        Err(JobError::QueueFull { .. }) => TickOutcome::Unchanged, // retry later
        Err(_) => TickOutcome::Stopped,                            // runner stopped
    }
}

/// Refresh the shared snapshot and notify `on_change` (never under the
/// lock).
fn publish(
    shared: &Arc<Mutex<SyncSchedulerStatus>>,
    on_change: &(dyn Fn(&SyncSchedulerStatus) + Send + Sync),
    snapshot: SyncSchedulerStatus,
) {
    *lock_status(shared) = snapshot.clone();
    on_change(&snapshot);
}
