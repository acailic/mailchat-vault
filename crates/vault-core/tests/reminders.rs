//! A9 proof tests: the manual-export reminder core's contract (plan
//! §5.4).
//!
//! - opt-in: a fresh vault schedules nothing, and absence stays silent;
//! - enabling arms exactly one deadline one full interval out — never
//!   "remind now", never a grid;
//! - the preference and its deadline persist in the vault: a reminder
//!   whose deadline passed while the app was closed fires at the next
//!   launch — and however many deadlines passed, exactly one reminder
//!   per source is due (no burst);
//! - snooze defers a due reminder without moving the schedule; dismiss
//!   acknowledges it and re-arms one interval out;
//! - disabling silences the source without deleting anything.
//!
//! Every test drives the real DbPool (writer, readers) over a real
//! SQLite file; only the clock is manual, jumping days the way an app
//! that stayed closed would.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::reminders::{
    evaluate, ManualWallClock, ReminderConfig, ReminderCore, ReminderPreference, ReminderStatus,
};
use vault_core::storage::Storage;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// A generous upper bound for the day-scale cadences under test
/// (mirrors the scheduler suite's fast config: tiny minimum so clamps
/// under test can fire, no product bound in the way).
const TEST_MAX_MS: u64 = 365 * DAY_MS;

fn pool(tmp: &tempfile::TempDir) -> Arc<DbPool> {
    DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap()
}

fn fast_config(clock: &ManualWallClock) -> ReminderConfig {
    ReminderConfig {
        clock: clock.clock(),
        min_interval_ms: 1,
        max_interval_ms: TEST_MAX_MS,
    }
}

fn core(pool: &Arc<DbPool>, clock: &ManualWallClock) -> ReminderCore {
    ReminderCore::new(Arc::clone(pool), fast_config(clock))
}

fn clock_at_2026_start() -> ManualWallClock {
    ManualWallClock::new(Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap())
}

/// Format an instant the way every serialized reminder fact does.
fn rfc(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Opt-in and persistence
// ---------------------------------------------------------------------------

#[test]
fn fresh_vault_schedules_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Storage::open(tmp.path().join("vault.db")).unwrap();
    assert!(
        s.reminder_preferences().unwrap().is_empty(),
        "opt-in: a fresh vault has no reminder preferences"
    );

    let clock = clock_at_2026_start();
    let c = core(&pool(&tmp), &clock);
    assert!(
        c.due().unwrap().is_empty(),
        "opt-in: nothing is due without a preference"
    );
    assert!(
        c.status("whatsapp/main").unwrap().is_none(),
        "a source that never opted in has no status"
    );
}

#[test]
fn preference_roundtrips_through_storage_and_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("vault.db");
    let base = Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap();
    let pref = ReminderPreference {
        source_instance_id: "whatsapp/main".to_string(),
        enabled: true,
        interval_ms: 30 * DAY_MS,
        next_due_at: Some(base + chrono::Duration::days(30)),
        snoozed_until: None,
    };
    {
        let s = Storage::open(&db).unwrap();
        let stored = s.set_reminder_preference(pref.clone()).unwrap();
        assert_eq!(stored, pref);
        assert_eq!(
            s.reminder_preference("whatsapp/main").unwrap(),
            Some(pref.clone())
        );
    }

    // Reopen: the same preference reads back — scheduling facts survive
    // restart by construction (this is the durable home the core arms
    // from).
    let s = Storage::open(&db).unwrap();
    assert_eq!(
        s.reminder_preference("whatsapp/main").unwrap(),
        Some(pref.clone())
    );
    assert_eq!(s.reminder_preferences().unwrap(), vec![pref]);
}

// ---------------------------------------------------------------------------
// Arming and the single deadline
// ---------------------------------------------------------------------------

#[test]
fn enable_arms_one_full_interval_out() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);

    let status = c.enable("telegram/main", 7 * DAY_MS).unwrap();
    assert!(status.enabled);
    assert!(!status.due, "enabling is not 'remind now'");
    assert_eq!(
        status.next_due_at,
        Some(rfc(Utc.with_ymd_and_hms(2026, 1, 8, 9, 0, 0).unwrap())),
        "the deadline is exactly one interval after the arm"
    );

    // Nothing is due yet, and the status read back shows the arm that
    // persisted (due-ness is derived, so it stays false until the
    // deadline passes).
    assert!(c.due().unwrap().is_empty());
    let again = c.status("telegram/main").unwrap().unwrap();
    assert!(!again.due);
    assert_eq!(again, status);

    pool.shutdown();
}

#[test]
fn intervals_clamp_to_the_configured_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = ReminderCore::new(
        Arc::clone(&pool),
        ReminderConfig {
            min_interval_ms: 10,
            max_interval_ms: 20,
            ..fast_config(&clock)
        },
    );

    let clamped = c.enable("signal/main", 0).unwrap();
    assert_eq!(clamped.interval_ms, 10);
    let clamped = c.enable("signal/main", 1_000).unwrap();
    assert_eq!(clamped.interval_ms, 20);

    // The clamp is what gets persisted.
    let stored = pool
        .with_read(|s| s.reminder_preference("signal/main"))
        .unwrap()
        .unwrap();
    assert_eq!(stored.interval_ms, 20);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Restart: fires on next launch, collapses to one reminder
// ---------------------------------------------------------------------------

#[test]
fn reminder_fires_after_simulated_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let first = pool(&tmp);
    core(&first, &clock)
        .enable("whatsapp/main", 30 * DAY_MS)
        .unwrap();

    // The app stays closed over the deadline: two days past it.
    clock.advance(chrono::Duration::days(32));
    first.shutdown();

    // Next launch: a fresh pool and core over the same vault observe
    // the due reminder.
    let relaunched = pool(&tmp);
    let c = core(&relaunched, &clock);
    let due = c.due().unwrap();
    assert_eq!(due.len(), 1, "exactly one due reminder after restart");
    assert_eq!(due[0].source_instance_id, "whatsapp/main");
    assert!(due[0].due);
    assert!(!due[0].snoozed);

    // Evaluation is stable: asking again presents the same one
    // reminder, not a growing pile.
    assert_eq!(c.due().unwrap().len(), 1);
    relaunched.shutdown();
}

#[test]
fn a_long_closure_collapses_to_one_reminder_not_a_burst() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let first = pool(&tmp);
    core(&first, &clock)
        .enable("whatsapp/main", 30 * DAY_MS)
        .unwrap();

    // Ten intervals pass with the app closed: however many deadlines a
    // grid would have produced, a single instant cannot queue them.
    clock.advance(chrono::Duration::days(300));
    first.shutdown();

    let relaunched = pool(&tmp);
    let c = core(&relaunched, &clock);
    let due = c.due().unwrap();
    assert_eq!(
        due.len(),
        1,
        "a long closure must collapse into one reminder, got {due:?}"
    );
    assert_eq!(due[0].source_instance_id, "whatsapp/main");
    relaunched.shutdown();
}

// ---------------------------------------------------------------------------
// Snooze and dismiss
// ---------------------------------------------------------------------------

#[test]
fn snooze_defers_without_advancing_the_schedule() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);
    c.enable("whatsapp/main", 30 * DAY_MS).unwrap();

    // Due on launch after the deadline passed.
    clock.advance(chrono::Duration::days(31));
    assert_eq!(c.due().unwrap().len(), 1);
    let armed_at = c.status("whatsapp/main").unwrap().unwrap().next_due_at;

    // Snooze one day: deferred, and the schedule itself did not move.
    let status = c.snooze("whatsapp/main", DAY_MS).unwrap();
    assert!(status.snoozed, "a due reminder under snooze reports it");
    assert!(!status.due);
    assert!(c.due().unwrap().is_empty(), "a snoozed reminder is silent");
    assert_eq!(
        status.next_due_at, armed_at,
        "snoozing must not advance the schedule"
    );

    // The snooze passes: due again, exactly once.
    clock.advance(chrono::Duration::days(2));
    let due = c.due().unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].next_due_at, armed_at);
    pool.shutdown();
}

#[test]
fn dismiss_acknowledges_and_re_arms_one_interval_out() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);
    c.enable("whatsapp/main", 30 * DAY_MS).unwrap();

    clock.advance(chrono::Duration::days(45));
    assert_eq!(c.due().unwrap().len(), 1);
    let dismissed_at = Utc.with_ymd_and_hms(2026, 2, 15, 9, 0, 0).unwrap();
    assert_eq!((clock.clock())(), dismissed_at);

    let status = c.dismiss("whatsapp/main").unwrap();
    assert!(!status.due);
    assert!(
        status.snoozed_until.is_none(),
        "dismissal clears any snooze"
    );
    assert_eq!(
        status.next_due_at,
        Some(rfc(dismissed_at + chrono::Duration::days(30))),
        "one acknowledgment buys one full interval of quiet"
    );
    assert!(c.due().unwrap().is_empty());

    // The re-armed deadline is honest: it fires when it passes.
    clock.advance(chrono::Duration::days(31));
    assert_eq!(c.due().unwrap().len(), 1);
    pool.shutdown();
}

#[test]
fn snooze_then_dismiss_clears_the_snooze() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);
    c.enable("telegram/main", 7 * DAY_MS).unwrap();

    clock.advance(chrono::Duration::days(8));
    assert_eq!(c.due().unwrap().len(), 1);

    // Snooze, then change your mind and dismiss: the snooze must not
    // resurrect after the new deadline passes.
    c.snooze("telegram/main", 3 * DAY_MS).unwrap();
    let status = c.dismiss("telegram/main").unwrap();
    assert!(!status.due);
    assert!(status.snoozed_until.is_none());

    clock.advance(chrono::Duration::days(4));
    assert!(
        c.due().unwrap().is_empty(),
        "the dismissed reminder must stay quiet for its full interval"
    );
    clock.advance(chrono::Duration::days(4));
    assert_eq!(c.due().unwrap().len(), 1, "re-armed on schedule");
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Disable and caller errors
// ---------------------------------------------------------------------------

#[test]
fn disable_silences_a_due_reminder() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);
    c.enable("whatsapp/main", 30 * DAY_MS).unwrap();

    clock.advance(chrono::Duration::days(31));
    assert_eq!(c.due().unwrap().len(), 1);

    let status = c.disable("whatsapp/main").unwrap();
    assert!(!status.enabled);
    assert!(!status.due, "a disabled row is never due");
    assert!(c.due().unwrap().is_empty());

    // The row survives (opt-out keeps state; it deletes nothing).
    let stored = pool
        .with_read(|s| s.reminder_preference("whatsapp/main"))
        .unwrap();
    assert!(stored.is_some(), "disabling must not delete the preference");
    pool.shutdown();
}

#[test]
fn transitions_on_unknown_sources_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = clock_at_2026_start();
    let pool = pool(&tmp);
    let c = core(&pool, &clock);

    assert!(c.snooze("ghost/main", DAY_MS).is_err());
    assert!(c.dismiss("ghost/main").is_err());
    // Errors, not accidental writes.
    assert!(pool
        .with_read(|s| s.reminder_preference("ghost/main"))
        .unwrap()
        .is_none());
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// The pure evaluation seam
// ---------------------------------------------------------------------------

#[test]
fn evaluation_is_a_pure_function_of_preference_and_clock() {
    let t0 = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
    let pref = |enabled, next_due_at, snoozed_until| ReminderPreference {
        source_instance_id: "s".to_string(),
        enabled,
        interval_ms: 30 * DAY_MS,
        next_due_at,
        snoozed_until,
    };
    let due_at = t0 + chrono::Duration::days(1);

    // Disabled rows are never due, whatever the deadline says.
    assert!(!evaluate(&pref(false, Some(t0), None), due_at).due);
    // Armed and past the deadline: due.
    assert!(evaluate(&pref(true, Some(t0), None), due_at).due);
    // Not yet: silent.
    assert!(!evaluate(&pref(true, Some(due_at), None), t0).due);
    // A live snooze defers; an expired one does not.
    let snoozed = evaluate(
        &pref(true, Some(t0), Some(due_at + chrono::Duration::days(1))),
        due_at,
    );
    assert!(!snoozed.due && snoozed.snoozed);
    let expired = evaluate(&pref(true, Some(t0), Some(due_at)), due_at);
    assert!(expired.due && !expired.snoozed);
    // A never-armed row is silent.
    assert!(!evaluate(&pref(true, None, None), due_at).due);
    // And the status echoes the persisted facts it derives from
    // (evaluating at the deadline itself: due).
    let status = evaluate(&pref(true, Some(t0), None), t0);
    assert_eq!(
        status,
        ReminderStatus {
            source_instance_id: "s".to_string(),
            enabled: true,
            interval_ms: 30 * DAY_MS,
            next_due_at: Some(rfc(t0)),
            snoozed_until: None,
            due: true,
            snoozed: false,
        }
    );
}
