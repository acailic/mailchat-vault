//! M2.1 proof tests: the database pool's concurrency contract, the typed
//! job lifecycle, polling recovery of missed events, bounded-queue
//! backpressure, and the network/write-lane separation of pool-marshaled
//! IMAP sync.
//!
//! Every test runs the real threads (writer, two readers, job runner) over
//! a real SQLite file in WAL mode — no mocks except the in-process IMAP
//! server for the network-stall proof.

#[path = "imap_mock/mod.rs"]
mod imap_mock;

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use imap_mock::{MockImap, MockMailbox};
use serde_json::{json, Value};
use vault_core::imap as vimap;
use vault_core::jobs::{
    DbPool, JobError, JobEventDetail, JobEventsPage, JobId, JobState, PoolConfig, PoolSync,
};
use vault_core::message::NewMessage;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn config(tiny_ring: Option<usize>) -> PoolConfig {
    PoolConfig {
        event_ring: tiny_ring.unwrap_or(1024),
        ..PoolConfig::default()
    }
}

fn pool(tmp: &tempfile::TempDir) -> Arc<DbPool> {
    DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap()
}

/// Poll until `cond` holds or `timeout` elapses.
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn msg(id: u64, body: &str) -> NewMessage {
    NewMessage::new(
        "pool-test",
        format!("m{id}"),
        "ana",
        format!("2026-11-{:02}T10:00:00Z", (id % 28 + 1) as u32),
        body,
    )
}

fn seed(pool: &Arc<DbPool>, count: u64) {
    pool.submit_write(move |s| {
        for i in 0..count {
            s.insert_message(&msg(i, &format!("needle {i} body text")))?;
        }
        Ok(())
    })
    .unwrap();
}

/// A search read exactly as the UI issues it: FTS over the archive. Its
/// `total` is the hit count of the reader's current snapshot.
fn search_total(pool: &Arc<DbPool>) -> u64 {
    let query = build_match_query("needle");
    pool.with_read(|s| {
        s.search_page(&vault_core::query::SearchQuery {
            text: query,
            ..Default::default()
        })
        .unwrap()
        .total
    })
}

// ---------------------------------------------------------------------------
// Reads stay available while the writer works (the core M2.1 contract)
// ---------------------------------------------------------------------------

#[test]
fn search_works_while_writes_are_in_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    seed(&pool, 20);

    let write_done = Arc::new(AtomicBool::new(false));
    let writer_pool = Arc::clone(&pool);
    let done_flag = Arc::clone(&write_done);
    let writer = std::thread::spawn(move || {
        // One long write "job" (submit_write round trip): thousands of
        // individually-committed inserts keep the writer lane busy for a
        // while, like a large import would.
        writer_pool
            .submit_write(move |s| {
                for i in 1000..8000 {
                    s.insert_message(&msg(i, &format!("needle bulk {i}")))?;
                }
                Ok(())
            })
            .unwrap();
        done_flag.store(true, Ordering::SeqCst);
    });

    // Concurrently: searches must succeed and observe consistent snapshots
    // (a WAL reader sees the pre-write or a committed state, never a torn
    // one). At least one search must complete while the write is running.
    let mut searches_during_write = 0u64;
    let mut last_total = 0u64;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let total = search_total(&pool);
        assert!(
            total >= last_total,
            "WAL reader saw its snapshot go backwards: {total} < {last_total}"
        );
        last_total = total;
        if !write_done.load(Ordering::SeqCst) {
            searches_during_write += 1;
        } else {
            break;
        }
    }
    writer.join().unwrap();
    assert!(
        searches_during_write > 0,
        "no search completed while the writer was busy — reads are being serialized behind writes"
    );
    // The write fully landed.
    assert_eq!(search_total(&pool), 20 + 7000);
    pool.shutdown();
}

#[test]
fn two_reads_run_concurrently_on_the_two_read_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    seed(&pool, 5);

    // Both closures occupy a read slot for a moment; the high-water mark
    // of simultaneous occupants is 2 only if both connections admit a
    // reader at once (a single connection would cap it at 1).
    let inside = Arc::new(AtomicU64::new(0));
    let max_inside = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let pool = Arc::clone(&pool);
        let inside = Arc::clone(&inside);
        let max_inside = Arc::clone(&max_inside);
        handles.push(std::thread::spawn(move || {
            pool.with_read(|s| {
                let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
                max_inside.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                inside.fetch_sub(1, Ordering::SeqCst);
                s.stats().unwrap().total
            })
        }));
    }
    for handle in handles {
        let total = handle.join().unwrap();
        assert_eq!(total, 5);
    }
    assert_eq!(
        max_inside.load(Ordering::SeqCst),
        2,
        "two readers never held a read connection simultaneously"
    );
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Bounded queue: backpressure, ordering, single writer
// ---------------------------------------------------------------------------

#[test]
fn write_queue_is_bounded_and_backpressures() {
    let tmp = tempfile::tempdir().unwrap();
    // Capacity 1: one slot; a second in-flight submission must wait.
    let cfg = PoolConfig {
        write_queue_capacity: 1,
        ..PoolConfig::default()
    };
    let pool = DbPool::spawn(tmp.path().join("vault.db"), cfg, |_| {}).unwrap();

    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let first = {
        let pool = Arc::clone(&pool);
        std::thread::spawn(move || {
            pool.submit_write(move |_s| {
                started_tx.send(()).unwrap();
                // Hold the writer until the test says so.
                let _ = gate_rx.recv();
                Ok(1u64)
            })
            .unwrap()
        })
    };
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // The writer is busy inside op #1 and the queue slot is free, so op #2
    // is accepted into the bounded queue but cannot run; op #3 must WAIT
    // (backpressure) — proven by it not finishing before the gate opens.
    let second_started = Arc::new(AtomicBool::new(false));
    let third_done = Arc::new(AtomicBool::new(false));
    let second = {
        let pool = Arc::clone(&pool);
        let flag = Arc::clone(&second_started);
        std::thread::spawn(move || {
            pool.submit_write(move |_s| {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        })
    };
    let third = {
        let pool = Arc::clone(&pool);
        let done = Arc::clone(&third_done);
        std::thread::spawn(move || {
            let _ = pool.submit_write(|_s| Ok(()));
            done.store(true, Ordering::SeqCst);
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !third_done.load(Ordering::SeqCst),
        "a third write finished while the bounded queue was full — no backpressure"
    );

    gate_tx.send(()).unwrap();
    first.join().unwrap();
    second.join().unwrap();
    third.join().unwrap();
    assert!(second_started.load(Ordering::SeqCst));
    assert!(third_done.load(Ordering::SeqCst));
    pool.shutdown();
}

#[test]
fn writes_execute_in_submission_order_on_one_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let log = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    // All submissions from one thread: queue arrival order is exactly
    // 0..=15, and the single writer must execute them in that order.
    for i in 0..16u64 {
        let log = Arc::clone(&log);
        pool.submit_write(move |s| {
            s.insert_message(&msg(i, &format!("ordered {i}")))?;
            log.lock().unwrap().push(i);
            Ok(())
        })
        .unwrap();
    }
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        (0..16).collect::<Vec<_>>(),
        "writes ran out of arrival order"
    );
    assert_eq!(pool.with_read(|s| s.stats().unwrap().total), 16);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Typed job lifecycle
// ---------------------------------------------------------------------------

/// Wait for a job to reach a terminal state and return the record.
fn wait_terminal(pool: &Arc<DbPool>, id: JobId) -> vault_core::jobs::JobRecord {
    assert!(
        eventually(Duration::from_secs(10), || {
            pool.job(id).is_some_and(|r| r.state.is_terminal())
        }),
        "job never became terminal: {:?}",
        pool.job(id)
    );
    pool.job(id).unwrap()
}

#[test]
fn job_lifecycle_success_failure_and_typed_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);

    // Success: result value round-trips.
    let ok = pool
        .submit_job("test.echo", |ctx| {
            ctx.write(|s| s.insert_message(&msg(1, "needle via job")))?;
            Ok(json!({ "answer": 42 }))
        })
        .unwrap();
    let record = wait_terminal(&pool, ok);
    assert_eq!(
        record.state,
        JobState::Succeeded {
            result: json!({ "answer": 42 })
        }
    );
    assert_eq!(record.kind, "test.echo");
    assert!(record.started_at.is_some() && record.finished_at.is_some());
    assert_eq!(pool.with_read(|s| s.stats().unwrap().total), 1);

    // Failure: the error arrives typed, not as a string.
    let bad = pool
        .submit_job("test.boom", |_ctx| {
            Err(JobError::Import {
                message: "cannot parse".into(),
            })
        })
        .unwrap();
    let record = wait_terminal(&pool, bad);
    assert_eq!(
        record.state,
        JobState::Failed {
            error: JobError::Import {
                message: "cannot parse".into()
            }
        }
    );

    // A job returning a core error converts to a typed kind.
    let storage_err = pool
        .submit_job("test.storage_fail", |ctx| {
            let _ = ctx
                .write(|_s| Ok::<(), vault_core::Error>(()))
                .map_err(JobError::from);
            Err(vault_core::Error::Sqlite(rusqlite::Error::QueryReturnedNoRows).into())
        })
        .unwrap();
    let record = wait_terminal(&pool, storage_err);
    let JobState::Failed { error } = record.state else {
        panic!("expected failure");
    };
    assert!(
        matches!(error, JobError::Storage { .. }),
        "core storage error must map to a typed storage job error, got {error:?}"
    );

    // Unknown id: no record.
    assert!(pool.job(JobId(999_999)).is_none());
    pool.shutdown();
}

#[test]
fn job_cancellation_is_cooperative_and_typed() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);

    // The job runs until cancelled, checking its flag between batches.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let id = pool
        .submit_job("test.long", move |ctx| {
            let mut done = 0u64;
            loop {
                if ctx.is_cancelled() {
                    return Err(JobError::Cancelled);
                }
                if stop_rx.try_recv().is_ok() {
                    return Ok(json!({ "completed": done }));
                }
                let pending = msg(900 + done, "cancel probe needle");
                ctx.write(move |s| {
                    s.insert_message(&pending)?;
                    Ok(())
                })?;
                done += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .unwrap();

    assert!(eventually(Duration::from_secs(10), || {
        pool.job(id).is_some_and(|r| r.state == JobState::Running)
    }));
    assert!(pool.cancel_job(id));
    let record = wait_terminal(&pool, id);
    assert_eq!(record.state, JobState::Cancelled);
    // Cancelling again is a no-op (already terminal).
    assert!(!pool.cancel_job(id));
    drop(stop_tx);

    // A job cancelled while still queued never runs its body.
    let blocker_done = Arc::new(AtomicBool::new(false));
    let blocker_flag = Arc::clone(&blocker_done);
    let blocker = pool
        .submit_job("test.blocker", move |_ctx| {
            while !blocker_flag.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(Value::Null)
        })
        .unwrap();
    assert!(eventually(Duration::from_secs(10), || {
        pool.job(blocker)
            .is_some_and(|r| r.state == JobState::Running)
    }));
    let queued = pool
        .submit_job("test.queued_cancel", |ctx| {
            if ctx.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            Ok(Value::Null)
        })
        .unwrap();
    assert!(pool.cancel_job(queued));
    blocker_done.store(true, Ordering::SeqCst);
    let record = wait_terminal(&pool, queued);
    assert_eq!(
        record.state,
        JobState::Cancelled,
        "queued job cancelled before start must not run"
    );
    let _ = wait_terminal(&pool, blocker);
    pool.shutdown();
}

#[test]
fn job_progress_is_reported_and_visible_in_events() {
    let tmp = tempfile::tempdir().unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let pool = DbPool::spawn(
        tmp.path().join("vault.db"),
        PoolConfig::default(),
        move |e| seen.lock().unwrap().push(e.clone()),
    )
    .unwrap();
    let id = pool
        .submit_job("test.progress", |ctx| {
            for i in 1..=3u64 {
                let pending = msg(i, "progress needle");
                ctx.write(move |s| {
                    s.insert_message(&pending)?;
                    Ok(())
                })?;
                ctx.progress(i, 3, "counting");
            }
            Ok(Value::Null)
        })
        .unwrap();
    let record = wait_terminal(&pool, id);
    assert_eq!(record.progress.map(|p| (p.done, p.total)), Some((3, 3)));

    let events = events.lock().unwrap().clone();
    let progress: Vec<_> = events
        .iter()
        .filter(|e| e.job == id)
        .filter_map(|e| match &e.detail {
            JobEventDetail::Progress { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress, vec![(1, 3), (2, 3), (3, 3)]);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Event log: monotonic seqs, polling recovery, ring trimming
// ---------------------------------------------------------------------------

#[test]
fn event_seqs_are_monotonic_and_recoverable_by_polling() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    for i in 0..5 {
        let n = i;
        pool.submit_job("test.ping", move |_| Ok(json!({ "i": n })))
            .unwrap();
    }
    assert!(eventually(Duration::from_secs(10), || {
        pool.jobs().iter().all(|r| r.state.is_terminal())
    }));

    // Everything from zero: submitted → started → succeeded per job.
    let page = pool.events_since(0);
    let seqs: Vec<u64> = page.events.iter().map(|e| e.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "events must be ascending in seq");
    assert_eq!(
        seqs.first().copied(),
        Some(1),
        "first allocated seq is 1 (ids never reused)"
    );
    assert!(seqs.windows(2).all(|w| w[1] > w[0]));
    assert_eq!(page.events.len(), 15); // 3 events × 5 jobs
    assert!(!page.trimmed);
    assert_eq!(page.latest_seq, 15);

    // Polling with the latest cursor yields nothing new…
    let page = pool.events_since(15);
    assert!(page.events.is_empty());
    assert_eq!(page.latest_seq, 15);

    // …and a stale cursor (a UI that missed events) gets exactly the gap.
    let page = pool.events_since(9);
    let seqs: Vec<u64> = page.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (10..=15).collect::<Vec<_>>());
    // The snapshot travels with every page for state reconciliation.
    assert_eq!(page.jobs.len(), 5);
    pool.shutdown();
}

#[test]
fn ring_trim_is_reported_and_state_stays_authoritative() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = DbPool::spawn(tmp.path().join("vault.db"), config(Some(8)), |_| {}).unwrap();
    for i in 0..20u64 {
        let n = i;
        pool.submit_job("test.chatter", move |_| Ok(json!({ "i": n })))
            .unwrap();
    }
    assert!(eventually(Duration::from_secs(20), || {
        pool.jobs().iter().all(|r| r.state.is_terminal())
    }));

    // A cursor far behind the 8-event ring: the page says trimmed and the
    // client reconciles from the job snapshot instead of the timeline.
    let page: JobEventsPage = pool.events_since(0);
    assert!(page.trimmed, "ring of 8 with 60 events must report trimmed");
    assert_eq!(page.events.len(), 8);
    assert!(page.oldest_kept > 1);
    assert_eq!(page.jobs.len(), 20, "records survive ring trimming");
    assert!(
        page.jobs.iter().all(|r| r.state.is_terminal()),
        "snapshot must carry the authoritative terminal states"
    );

    // A cursor inside the retained window is served precisely.
    let in_window = page.oldest_kept + 2;
    let fresh = pool.events_since(in_window);
    assert!(!fresh.trimmed);
    assert_eq!(fresh.events.first().map(|e| e.seq), Some(in_window + 1));
    pool.shutdown();
}

#[test]
fn try_submit_reports_queue_full_without_waiting() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = PoolConfig {
        job_queue_capacity: 1,
        ..PoolConfig::default()
    };
    let pool = DbPool::spawn(tmp.path().join("vault.db"), cfg, |_| {}).unwrap();

    // Occupy the runner with a gated job; one slot still fits in the
    // queue; the third submission is over capacity.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (running_tx, running_rx) = mpsc::channel::<()>();
    let running = pool
        .submit_job("test.hold", move |_ctx| {
            running_tx.send(()).unwrap();
            let _ = release_rx.recv();
            Ok(Value::Null)
        })
        .unwrap();
    running_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    let queued = pool
        .try_submit_job("test.filler", move |_| Ok(Value::Null))
        .unwrap();
    let full = pool.try_submit_job("test.overflow", move |_| Ok(Value::Null));
    assert!(
        matches!(full, Err(JobError::QueueFull { .. })),
        "expected a typed queue-full error, got {full:?}"
    );
    // The overflowed submission failed terminally — a client polling the
    // job list is not left waiting on a job that can never start.
    assert!(eventually(Duration::from_secs(10), || {
        pool.jobs()
            .iter()
            .any(|r| matches!(r.state, JobState::Failed { .. }))
    }));

    release_tx.send(()).unwrap();
    let _ = wait_terminal(&pool, running);
    let _ = wait_terminal(&pool, queued);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// IMAP over the pool: network waits never occupy the write lane
// ---------------------------------------------------------------------------

const USER: &str = "alice@example.com";
const PASSWORD: &str = "pool-proof-password";
const MOCK_HOST: &str = "mock.test";

fn raw_mail(n: u64) -> Vec<u8> {
    format!(
        "From: Alice Smith <alice@example.com>\r\n\
         To: Bob <bob@example.com>\r\n\
         Subject: pool proof {n}\r\n\
         Message-ID: <pool-{n}@mock.test>\r\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         body of message {n} with the word needle"
    )
    .into_bytes()
}

/// The v2 `imap_sync` job shape: every storage call goes through the
/// pool adapter ([`PoolSync`]); the session (the network half) is built
/// by the caller — plain TCP against the mock here, TLS in production.
/// Both halves together are exactly what `imap::sync_with` does; the job
/// keeps them separate so the session can be injected in tests.
fn pool_imap_sync(
    ctx: &vault_core::jobs::JobContext<'_>,
    cfg: &vimap::ImapConfig,
    addr: std::net::SocketAddr,
) -> std::result::Result<Vec<vimap::FolderSyncSummary>, vault_core::Error> {
    use vault_core::imap::SyncStorage as _;
    let sink = PoolSync(ctx.pool());
    let instance = sink.ensure_instance(cfg)?;
    sink.record_attempt(&instance)?;
    let stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    let mut client = imap::Client::new(stream);
    client.read_greeting()?;
    // The pool-proof test exercises the password leg; the OAuth leg
    // authenticates via XOAUTH2 in the imap tests instead.
    let password = match &cfg.auth {
        vimap::ImapAuth::Password(password) => password.clone(),
        vimap::ImapAuth::Xoauth2 { access_token } => access_token.clone(),
    };
    let mut session = client
        .login(cfg.username.clone(), password)
        .map_err(|(e, _client)| e)?;
    let folders = cfg.effective_folders();
    let summaries = vimap::sync_session(&mut session, &sink, &instance, &folders, None);
    let _ = session.logout();
    sink.record_success(&instance)?;
    Ok(summaries)
}

#[test]
fn imap_sync_network_stall_does_not_block_writes_or_reads() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, (1..=120u32).map(|n| raw_mail(n as u64)).collect()),
    );
    // Stall the third UID FETCH (batches of 50): by then the sync has
    // committed two batches of writes and is parked on the network.
    server.stall_uid_fetch_once(3);

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    seed(&pool, 3);

    // The sync runs as a job (exactly what the v2 `imap_sync` job does).
    let stalled_writes = Arc::new(AtomicU64::new(0));
    let marker = Arc::clone(&stalled_writes);
    let addr = server.addr;
    let cfg = vimap::ImapConfig {
        host: MOCK_HOST.to_string(),
        port: server.addr.port(),
        username: USER.to_string(),
        auth: vimap::ImapAuth::Password(PASSWORD.to_string()),
        folders: vec!["INBOX".into()],
    };
    let job = pool
        .submit_job("imap_sync", move |ctx| {
            let summaries = pool_imap_sync(ctx, &cfg, addr)?;
            ctx.progress(1, 1, "synced");
            Ok(serde_json::to_value(&summaries).unwrap_or(Value::Null))
        })
        .unwrap();

    // Wait until the sync is parked on the stalled fetch: the first two
    // batches (100 mails) are committed by then.
    assert!(
        eventually(Duration::from_secs(10), || {
            pool.with_read(|s| s.stats().unwrap().total) >= 103
        }),
        "sync did not commit its first batches: {}",
        pool.with_read(|s| s.stats().unwrap().total)
    );

    // THE PROOF: while the network is stalled mid-sync, the write lane is
    // free — a plain write completes…
    let start = Instant::now();
    pool.submit_write(|s| {
        s.insert_message(&msg(4242, "needle written during imap stall"))?;
        Ok(())
    })
    .expect("a write must complete while the IMAP fetch is stalled on the network");
    marker.fetch_add(1, Ordering::SeqCst);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the write waited {:?} behind a stalled network fetch — network is occupying the write lane",
        start.elapsed()
    );
    // …and searches still answer.
    let hits = search_total(&pool);
    assert!(
        hits >= 103,
        "searches must work during the sync: {hits} hits"
    );

    // Release the network; the sync finishes and everything is stored.
    server.release_stall();
    let record = wait_terminal(&pool, job);
    let JobState::Succeeded { result } = record.state else {
        panic!("sync job failed: {record:?}");
    };
    assert_eq!(marker.load(Ordering::SeqCst), 1);
    let total = pool.with_read(|s| s.stats().unwrap().total);
    assert_eq!(total, 3 + 120 + 1, "all synced mails plus the stall write");
    assert!(result.is_array());
    pool.shutdown();
}

#[test]
fn imap_pool_sync_matches_direct_sync_archive() {
    // Equivalence: the same mailbox synced through the pool adapter and
    // through a direct connection produces the same archive counts.
    let server = MockImap::start();
    let server2 = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(3, vec![raw_mail(1), raw_mail(2)]));
    server2.set_mailbox("INBOX", MockMailbox::new(3, vec![raw_mail(1), raw_mail(2)]));

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    let addr = server.addr;
    let cfg = vimap::ImapConfig {
        host: MOCK_HOST.to_string(),
        port: server.addr.port(),
        username: USER.to_string(),
        auth: vimap::ImapAuth::Password(PASSWORD.to_string()),
        folders: vec!["INBOX".into()],
    };
    let job = pool
        .submit_job("imap_sync", move |ctx| {
            let summaries = pool_imap_sync(ctx, &cfg, addr)?;
            Ok(serde_json::to_value(&summaries).unwrap_or(Value::Null))
        })
        .unwrap();
    let record = wait_terminal(&pool, job);
    assert!(
        matches!(record.state, JobState::Succeeded { .. }),
        "{record:?}"
    );

    let direct = Storage::open(tmp.path().join("direct.db")).unwrap();
    let direct_summaries = {
        let mut session = {
            let stream = TcpStream::connect(server2.addr).unwrap();
            let mut client = imap::Client::new(stream);
            client.read_greeting().unwrap();
            client.login(USER, PASSWORD).map_err(|(e, _)| e).unwrap()
        };
        let folders = vec!["INBOX".to_string()];
        let instance = {
            let port = server2.addr.port();
            vimap::ensure_source_instance(
                &direct,
                &vimap::ImapConfig {
                    host: MOCK_HOST.to_string(),
                    port,
                    username: USER.to_string(),
                    auth: vimap::ImapAuth::Password(PASSWORD.to_string()),
                    folders: Vec::new(),
                },
            )
            .unwrap()
        };
        let summaries = vimap::sync_session(&mut session, &direct, &instance, &folders, None);
        let _ = session.logout();
        summaries
    };
    assert_eq!(direct_summaries[0].inserted, 2);
    assert_eq!(direct_summaries[0].error, None);
    assert_eq!(pool.with_read(|s| s.stats().unwrap().total), 2);
    assert_eq!(direct.stats().unwrap().total, 2);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// Regression (M2.1 e2e): the v2 protocol consumer (src/types.ts `JobEvent`)
/// reads a flat event object `{seq, at, job, type, ...}`. `JobEventDetail`
/// must stay serde-flattened into the event — a nested `detail` key leaves
/// `event.type` undefined in the frontend, so the jobs panel sticks on
/// "unknown — queued" while jobs actually run to completion.
#[test]
fn job_event_serializes_flat_for_the_frontend() {
    let event = vault_core::jobs::JobEvent {
        seq: 7,
        at: "2026-11-03T00:00:00Z".to_string(),
        job: JobId(1),
        detail: JobEventDetail::Progress {
            done: 3,
            total: 8,
            message: "embedding".to_string(),
        },
    };
    let wire = serde_json::to_value(&event).unwrap();
    assert_eq!(wire["type"], json!("progress"));
    assert_eq!(wire["job"], json!(1));
    assert_eq!(wire["done"], json!(3));
    assert!(
        wire.get("detail").is_none(),
        "detail must be flattened onto the event object, not nested: {wire}"
    );
}
