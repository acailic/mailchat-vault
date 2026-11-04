//! M2.4 proof tests: the opt-in periodic sync scheduler's contract.
//!
//! - rounds are ordinary pool jobs (kind `imap_sync_all`), one at a time;
//! - a round that outlasts the interval delays the next round — never
//!   overlaps it, never queues a stack of missed rounds behind it;
//! - a sleep/offline gap (simulated by jumping the injected clock across
//!   several missed intervals) collapses into exactly one catch-up round;
//! - disabling cancels the active round and stops submissions;
//! - the schedule persists in the vault and re-arms a fresh scheduler.
//!
//! Every test runs the real threads (writer, readers, job runner,
//! scheduler loop) over a real SQLite file; only the round body is a
//! mock (recording invocations, optionally blocking until released, and
//! honouring cooperative cancellation).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vault_core::jobs::{DbPool, JobError, JobId, JobState, PoolConfig};
use vault_core::scheduler::{
    ManualClock, RoundFn, SchedulerConfig, SyncSchedule, SyncScheduler, DEFAULT_INTERVAL_MS,
    MAX_INTERVAL_MS, MIN_INTERVAL_MS, SCHEDULED_JOB_KIND,
};
use vault_core::storage::Storage;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn pool(tmp: &tempfile::TempDir) -> Arc<DbPool> {
    DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap()
}

/// Test cadences in real time: sub-second intervals, 10 ms ticks, and
/// interval bounds shrunk so the clamps under test can fire.
fn fast_config() -> SchedulerConfig {
    SchedulerConfig {
        tick: Duration::from_millis(10),
        min_interval_ms: 1,
        max_interval_ms: 3_600_000,
        ..SchedulerConfig::default()
    }
}

/// Poll until `cond` holds or `timeout` elapses.
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// Records round invocations: how many started, whether any two ever
/// overlapped, and the instant each started (for cadence assertions).
#[derive(Default)]
struct Spy {
    started: AtomicU64,
    finished: AtomicU64,
    overlapped: AtomicBool,
    starts: Mutex<Vec<Instant>>,
}

impl Spy {
    fn enter(&self) {
        let n = self.started.fetch_add(1, Ordering::SeqCst) + 1;
        if n != self.finished.load(Ordering::SeqCst) + 1 {
            self.overlapped.store(true, Ordering::SeqCst);
        }
        self.starts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Instant::now());
    }

    fn exit(&self) {
        self.finished.fetch_add(1, Ordering::SeqCst);
    }

    fn started(&self) -> u64 {
        self.started.load(Ordering::SeqCst)
    }
}

/// A fast round: records entry/exit and succeeds immediately.
fn fast_round(spy: Arc<Spy>) -> RoundFn {
    Arc::new(move |_ctx| {
        spy.enter();
        spy.exit();
        Ok(json!({ "round": "fast" }))
    })
}

/// A round that parks until released (or cancelled): the shape of a
/// coordinator round stalled on a slow network. Later invocations — after
/// the release channel is spent — succeed immediately, so the catch-up
/// round after a long round still completes.
fn blocking_round(spy: Arc<Spy>, release: mpsc::Receiver<()>) -> RoundFn {
    // `Receiver` is not `Sync`; the round body must be. One mutex keeps
    // the wait (and the release) serialized across invocations.
    let release = Mutex::new(release);
    Arc::new(move |ctx| {
        spy.enter();
        let give_up = Instant::now() + Duration::from_secs(30);
        loop {
            if ctx.is_cancelled() {
                spy.exit();
                return Err(JobError::Cancelled);
            }
            let release = release.lock().unwrap_or_else(|p| p.into_inner());
            let received = release.recv_timeout(Duration::from_millis(5));
            drop(release);
            match received {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    spy.exit();
                    return Ok(json!({ "round": "released" }));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() > give_up {
                spy.exit();
                return Ok(json!({ "round": "timeout" }));
            }
        }
    })
}

fn enable(
    scheduler: &SyncScheduler,
    interval_ms: u64,
) -> vault_core::scheduler::SyncSchedulerStatus {
    scheduler
        .set(SyncSchedule {
            enabled: true,
            interval_ms,
        })
        .expect("enable schedule")
}

// ---------------------------------------------------------------------------
// The persisted schedule
// ---------------------------------------------------------------------------

#[test]
fn schedule_defaults_to_off_at_15_minutes() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path().join("vault.db")).unwrap();
    let schedule = s.sync_schedule().unwrap();
    assert!(!schedule.enabled, "opt-in: a fresh vault schedules nothing");
    assert_eq!(schedule.interval_ms, DEFAULT_INTERVAL_MS);
    assert_eq!(DEFAULT_INTERVAL_MS, 15 * 60 * 1000);
}

#[test]
fn schedule_persists_and_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("vault.db");
    let s = Storage::open(&db).unwrap();
    let stored = s
        .set_sync_schedule(SyncSchedule {
            enabled: true,
            interval_ms: 3_600_000,
        })
        .unwrap();
    assert_eq!(
        stored,
        SyncSchedule {
            enabled: true,
            interval_ms: 3_600_000
        }
    );

    // Reopen: the same schedule reads back (the scheduler re-arms from it).
    let s2 = Storage::open(&db).unwrap();
    assert_eq!(s2.sync_schedule().unwrap(), stored);
}

#[test]
fn scheduler_clamps_interval_to_its_configured_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        SchedulerConfig::default(),
        fast_round(Arc::new(Spy::default())),
        |_| {},
    );

    // Product bounds: 1 min ..= 24 h.
    let status = scheduler
        .set(SyncSchedule {
            enabled: false,
            interval_ms: 1,
        })
        .unwrap();
    assert_eq!(status.interval_ms, MIN_INTERVAL_MS);
    let status = scheduler
        .set(SyncSchedule {
            enabled: false,
            interval_ms: 10 * MAX_INTERVAL_MS,
        })
        .unwrap();
    assert_eq!(status.interval_ms, MAX_INTERVAL_MS);

    // The clamp is what gets persisted.
    let stored = pool.with_read(|s| s.sync_schedule()).unwrap();
    assert_eq!(stored.interval_ms, MAX_INTERVAL_MS);
    scheduler.shutdown();
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// The scheduling contract
// ---------------------------------------------------------------------------

#[test]
fn persisted_schedule_arms_with_normalized_interval() {
    for (stored_ms, expected_ms) in [(0, 1000), (10_000, 2000)] {
        let tmp = tempfile::tempdir().unwrap();
        let pool = pool(&tmp);
        pool.submit_write(move |s| {
            s.set_sync_schedule(SyncSchedule {
                enabled: true,
                interval_ms: stored_ms,
            })
        })
        .unwrap();
        let (tx, rx) = mpsc::channel();
        let scheduler = SyncScheduler::spawn(
            Arc::clone(&pool),
            SchedulerConfig {
                min_interval_ms: 1000,
                max_interval_ms: 2000,
                ..fast_config()
            },
            fast_round(Arc::new(Spy::default())),
            move |status| {
                let _ = tx.send(status.clone());
            },
        );
        let status = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        scheduler.shutdown();
        pool.shutdown();
        assert_eq!(status.interval_ms, expected_ms);
        assert_eq!(
            status.next_due_in_secs,
            Some(expected_ms / 1000),
            "the initial deadline must use the normalized interval, stored {stored_ms}ms"
        );
    }
}

#[test]
fn rounds_fire_per_interval_as_plain_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        fast_config(),
        fast_round(Arc::clone(&spy)),
        |_| {},
    );

    // Disabled: nothing fires even after many ticks.
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(spy.started(), 0, "opt-in: no round while disabled");

    let interval_ms = 80u64;
    let status = enable(&scheduler, interval_ms);
    assert!(status.enabled);
    assert_eq!(status.next_due_in_secs, Some(1), "80ms rounds up to ~1s");

    // ~5 intervals: at least 3 rounds, submitted through the job system
    // under the coordinator's job kind.
    assert!(
        eventually(Duration::from_millis(600), || spy.started() >= 3),
        "expected several scheduled rounds, got {}",
        spy.started()
    );
    assert!(
        pool.jobs().iter().any(|j| j.kind == SCHEDULED_JOB_KIND),
        "scheduled rounds are ordinary imap_sync_all jobs"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst), "rounds overlapped");

    // Cadence: consecutive rounds are never closer than one interval
    // (minus scheduling slack of two ticks).
    let starts = spy.starts.lock().unwrap_or_else(|p| p.into_inner()).clone();
    for pair in starts.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(interval_ms - 20),
            "rounds fired {gap:?} apart, closer than the {interval_ms}ms interval"
        );
    }

    scheduler.shutdown();
    pool.shutdown();
}

#[test]
fn round_outlasting_interval_neither_overlaps_nor_queues() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        fast_config(),
        blocking_round(Arc::clone(&spy), release_rx),
        |_| {},
    );

    // A 70ms cadence with a round that parks: the interval passes several
    // times while round 1 is still running.
    enable(&scheduler, 70);
    assert!(
        eventually(Duration::from_millis(500), || spy.started() == 1),
        "first round never fired"
    );
    assert!(
        scheduler.status().active_job.is_some(),
        "the running round is tracked"
    );

    // 300ms ≈ 4 intervals elapse with the round still active.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        spy.started(),
        1,
        "a round outlasting the interval must not stack another behind it"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));

    // Release: exactly one catch-up round fires at the next tick after
    // the first round ends, then the cadence resumes — no queue of the
    // ~4 missed intervals discharges. (The sender is dropped so the
    // catch-up round — which re-enters the same mock — returns
    // immediately instead of parking.)
    release_tx.send(()).unwrap();
    drop(release_tx);
    assert!(
        eventually(Duration::from_millis(500), || spy.started() == 2),
        "no catch-up round after the long round finished"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));
    // The cadence re-anchored at the catch-up submission: no third round
    // may follow within one interval of it (an instant follower would be
    // the missed-interval queue discharging).
    std::thread::sleep(Duration::from_millis(55));
    assert_eq!(
        spy.started(),
        2,
        "rounds fired instantly after the catch-up — a queue discharged"
    );
    // And the cadence does resume one interval after the catch-up.
    assert!(
        eventually(Duration::from_millis(500), || spy.started() >= 3),
        "cadence never resumed after the catch-up"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));

    scheduler.shutdown();
    pool.shutdown();
}

#[test]
fn sleep_or_offline_gap_collapses_to_one_catchup_round() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let clock = ManualClock::new(Instant::now());
    let config = SchedulerConfig {
        tick: Duration::from_millis(10),
        clock: clock.clock(),
        min_interval_ms: 1,
        max_interval_ms: 3_600_000,
    };
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        config,
        blocking_round(Arc::clone(&spy), release_rx),
        |_| {},
    );

    // A 60-second cadence on the manual clock: the test never waits real
    // minutes — time jumps instead, exactly like suspend/resume or an
    // offline gap looks to a deadline-based loop.
    enable(&scheduler, 60_000);
    clock.advance(Duration::from_secs(60));
    assert!(
        eventually(Duration::from_millis(500), || spy.started() == 1),
        "first round never fired"
    );

    // Ten intervals pass while round 1 is still parked (the machine was
    // "asleep", or the network was gone): the deadline is long past, but
    // the active round shields the queue — nothing is submitted.
    clock.advance(Duration::from_secs(600));
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        spy.started(),
        1,
        "a passing deadline must not submit while a round is active"
    );

    // The gap ends: exactly one round catches up — not ten, not a burst.
    // (The sender is dropped so the catch-up round — which re-enters the
    // same mock — returns immediately instead of parking.)
    release_tx.send(()).unwrap();
    drop(release_tx);
    assert!(
        eventually(Duration::from_millis(500), || spy.started() == 2),
        "no catch-up round after the gap"
    );
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        spy.started(),
        2,
        "offline period must not cause an avalanche of scheduled rounds"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));
    // The cadence re-anchored at the catch-up submission: the next round
    // is a full interval away on the (unmoved) clock.
    assert_eq!(scheduler.status().next_due_in_secs, Some(60));

    scheduler.shutdown();
    pool.shutdown();
}

#[test]
fn disabling_cancels_the_active_round_and_stops_submissions() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        fast_config(),
        blocking_round(Arc::clone(&spy), release_rx),
        |_| {},
    );

    enable(&scheduler, 2_000);
    assert!(
        eventually(Duration::from_millis(3_000), || spy.started() == 1),
        "first round never fired"
    );
    let job = scheduler.status().active_job.expect("active round tracked");

    // Disable while the round is parked: the schedule turns off, the
    // active round is cancelled (cooperatively), nothing fires after.
    let status = scheduler
        .set(SyncSchedule {
            enabled: false,
            interval_ms: 2_000,
        })
        .unwrap();
    assert!(!status.enabled);
    assert_eq!(status.next_due_in_secs, None);

    let pool_for_wait = Arc::clone(&pool);
    let cancelled = eventually(Duration::from_millis(1000), || {
        pool_for_wait
            .job(JobId(job))
            .map(|r| r.state == JobState::Cancelled)
            .unwrap_or(false)
    });
    assert!(cancelled, "disabling must cancel the active round");
    drop(release_tx); // the round already returned; the channel is spent

    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(spy.started(), 1, "no rounds after disabling");
    assert!(!spy.overlapped.load(Ordering::SeqCst));

    scheduler.shutdown();
    pool.shutdown();
}

#[test]
fn spawn_arms_from_the_persisted_schedule_one_interval_out() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    // Persist an enabled schedule before any scheduler exists.
    pool.submit_write(move |s| {
        s.set_sync_schedule(SyncSchedule {
            enabled: true,
            interval_ms: 100,
        })
    })
    .unwrap();

    let spy = Arc::new(Spy::default());
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        fast_config(),
        fast_round(Arc::clone(&spy)),
        |_| {},
    );

    // The fresh scheduler arms from the vault but does not fire at spawn:
    // the first round comes one interval after the start, never an
    // immediate "catch-up" for time the app was not running.
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(spy.started(), 0, "spawn must not fire an immediate round");
    assert!(
        eventually(Duration::from_millis(500), || spy.started() >= 1),
        "persisted schedule never re-armed"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));

    scheduler.shutdown();
    pool.shutdown();
}

#[test]
fn failed_rounds_keep_the_cadence() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    // Every round fails (like an offline server): the scheduler must keep
    // submitting one round per interval regardless — each round carries
    // its own failure, the cadence is the scheduler's only concern.
    let failing: RoundFn = {
        let spy = Arc::clone(&spy);
        Arc::new(move |_ctx| {
            spy.enter();
            spy.exit();
            Err(JobError::Network {
                message: "server unreachable".into(),
            })
        })
    };
    let scheduler = SyncScheduler::spawn(Arc::clone(&pool), fast_config(), failing, |_| {});

    enable(&scheduler, 300);
    assert!(
        eventually(Duration::from_millis(1500), || spy.started() >= 3),
        "failed rounds must not stall the schedule"
    );
    assert!(!spy.overlapped.load(Ordering::SeqCst));

    scheduler.shutdown();
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Status publishing
// ---------------------------------------------------------------------------

#[test]
fn status_publishes_transitions() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let spy = Arc::new(Spy::default());
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let scheduler = SyncScheduler::spawn(
        Arc::clone(&pool),
        fast_config(),
        fast_round(Arc::clone(&spy)),
        move |status: &vault_core::scheduler::SyncSchedulerStatus| {
            sink.lock().unwrap_or_else(|p| p.into_inner()).push((
                status.enabled,
                status.active_job,
                status.rounds_submitted,
            ))
        },
    );

    enable(&scheduler, 500);
    assert!(
        eventually(Duration::from_millis(1500), || spy.started() >= 2),
        "rounds never fired"
    );
    let events = events.lock().unwrap_or_else(|p| p.into_inner()).clone();
    // The published stream carries the arm and the round submissions —
    // enough for a UI to render "next round in ~N min" without polling.
    assert!(events.iter().any(|&(enabled, _, _)| enabled));
    assert!(
        events
            .iter()
            .any(|&(_, active, rounds)| active.is_some() && rounds >= 1),
        "a submitted round is published with its job id"
    );

    scheduler.shutdown();
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Result plumbing sanity: the round's value is the job's result
// ---------------------------------------------------------------------------

#[test]
fn round_result_reaches_the_job_record() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let payload = json!({ "rounds": 1, "sources": [] });
    let value: Value = payload.clone();
    let round: RoundFn = Arc::new(move |_ctx| Ok(value.clone()));
    let scheduler = SyncScheduler::spawn(Arc::clone(&pool), fast_config(), round, |_| {});

    enable(&scheduler, 200);
    let got = eventually(Duration::from_millis(800), || {
        pool.jobs().iter().any(|j| match &j.state {
            JobState::Succeeded { result } => *result == payload,
            _ => false,
        })
    });
    assert!(
        got,
        "the scheduled round's result must reach the job record"
    );

    scheduler.shutdown();
    pool.shutdown();
}
