//! The manual-export reminder core (A9, plan §5.4): opt-in, device-
//! local, content-free nudges — one per source — that a manual export
//! might be due again ("has it been a month since your last WhatsApp
//! import?").
//!
//! # Reminder contract (the invariants this module is built on)
//!
//! - **Opt-in.** No preference row exists for a source until the user
//!   enables a reminder ([`ReminderCore::enable`]); absence means
//!   silence, and the row persists in the vault database (migration 25)
//!   — the same durable home the sync schedule has.
//! - **Device-local and content-free.** A reminder carries scheduling
//!   facts only: enabled, interval, the single deadline, the snooze
//!   state. No message content, no subject lines, no counts — the
//!   presentation layer may add context it can already read elsewhere,
//!   but the reminder itself never stores any.
//! - **One deadline; missed time collapses to one reminder.**
//!   [`ReminderPreference::next_due_at`] is a single instant — always
//!   `arm + interval`, never a wall-clock schedule grid — so however
//!   long the app stayed closed, at most one reminder per source is
//!   due at the next evaluation. There is no queue to discharge.
//! - **Fires on next launch; never after Quit.** The core owns no
//!   thread and submits no jobs (unlike [`crate::scheduler`]):
//!   due-ness is *evaluated* — at launch, or on any tick the shell
//!   chooses — against the persisted deadline. Nothing here promises
//!   OS background execution while the app is not running.
//! - **Snooze defers; dismiss advances.** Snoozing ([`ReminderCore::snooze`])
//!   sets [`ReminderPreference::snoozed_until`]: the reminder is
//!   presented again once that instant passes, and the schedule itself
//!   does not move. Dismissing ([`ReminderCore::dismiss`]) acknowledges
//!   the reminder: `next_due_at` becomes `now + interval` and any
//!   snooze clears — one acknowledgment, one interval of quiet.
//!
//! Time is read through an injectable wall clock ([`WallClock`],
//! [`ManualWallClock`]) so the regression suite can jump days; the
//! default reads the real UTC clock.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::jobs::DbPool;

/// Default interval between reminders (monthly, plan §5.4: "default
/// monthly when enabled").
pub const DEFAULT_INTERVAL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Shortest accepted interval (an hour — reminders are nudges, not a
/// metronome).
pub const MIN_INTERVAL_MS: u64 = 60 * 60 * 1000;
/// Longest accepted interval (one year).
pub const MAX_INTERVAL_MS: u64 = 365 * 24 * 60 * 60 * 1000;
/// Default snooze offered when the user defers a due reminder (a day).
pub const DEFAULT_SNOOZE_MS: u64 = 24 * 60 * 60 * 1000;

/// The persisted reminder preference for one source (migration 25):
/// scheduling facts only, content-free by construction. Timestamps are
/// `DateTime<Utc>` in memory; the storage seam (de)serializes them as
/// RFC 3339 UTC text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderPreference {
    pub source_instance_id: String,
    /// The user's opt-in. Off rows keep their scheduling state so a
    /// later re-enable starts from a fresh arm, never a stale deadline.
    pub enabled: bool,
    /// Period between reminders, in milliseconds (kept inside the
    /// core's interval bounds by its transitions; the UI speaks days).
    pub interval_ms: u64,
    /// When the next reminder may be presented (UTC). `None` only on a
    /// row that never armed.
    pub next_due_at: Option<DateTime<Utc>>,
    /// While set and still in the future: the user snoozed the due
    /// reminder; present again at/after this instant without moving
    /// `next_due_at`.
    pub snoozed_until: Option<DateTime<Utc>>,
}

/// What evaluating one preference at a moment concludes: the persisted
/// facts plus the derived `due`/`snoozed` flags. What the tray or UI
/// renders is a pure function of this — no hidden process state.
/// Timestamps ride as RFC 3339 UTC strings (the crate's chrono has no
/// serde feature; every other serialized fact — the A1b health facts
/// included — carries timestamps the same way).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReminderStatus {
    pub source_instance_id: String,
    pub enabled: bool,
    pub interval_ms: u64,
    /// RFC 3339 UTC, or `None` on a never-armed row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_due_at: Option<String>,
    /// RFC 3339 UTC while a snooze is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<String>,
    /// A reminder for this source may be presented now.
    pub due: bool,
    /// A due reminder is currently deferred by a snooze that has not
    /// passed yet.
    pub snoozed: bool,
}

/// A wall-clock time source for evaluation and transitions. Injectable
/// so tests can jump days (simulating an app that stayed closed); the
/// default reads the real UTC clock.
pub type WallClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// A manually advanced wall clock for the regression suite: `advance`
/// jumps time without sleeping, which is how "the app was closed over
/// the deadline" is simulated.
#[derive(Clone, Debug)]
pub struct ManualWallClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualWallClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Jump the clock forward by `by` (monotonic: only forward).
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|p| p.into_inner());
        *now += by;
    }

    /// The injectable [`WallClock`] view of this manual clock.
    pub fn clock(&self) -> WallClock {
        let now = Arc::clone(&self.now);
        Arc::new(move || *now.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// Tuning knobs of the reminder core. Tests shrink the interval bounds
/// so the clamps under test can fire.
#[derive(Clone)]
pub struct ReminderConfig {
    pub clock: WallClock,
    /// Interval bounds applied when a preference is written
    /// (defaults: [`MIN_INTERVAL_MS`]..=[`MAX_INTERVAL_MS`]).
    pub min_interval_ms: u64,
    pub max_interval_ms: u64,
}

impl Default for ReminderConfig {
    fn default() -> Self {
        Self {
            clock: Arc::new(Utc::now),
            min_interval_ms: MIN_INTERVAL_MS,
            max_interval_ms: MAX_INTERVAL_MS,
        }
    }
}

/// Evaluate one preference at `now` (pure; see the module's reminder
/// contract). Due means: enabled, the deadline passed, and no
/// still-active snooze defers it.
pub fn evaluate(pref: &ReminderPreference, now: DateTime<Utc>) -> ReminderStatus {
    let armed = pref.next_due_at.is_some_and(|d| now >= d);
    let snooze_active = pref.snoozed_until.is_some_and(|s| now < s);
    let fmt = |t: Option<DateTime<Utc>>| {
        t.map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    };
    ReminderStatus {
        source_instance_id: pref.source_instance_id.clone(),
        enabled: pref.enabled,
        interval_ms: pref.interval_ms,
        next_due_at: fmt(pref.next_due_at),
        snoozed_until: fmt(pref.snoozed_until),
        due: pref.enabled && armed && !snooze_active,
        snoozed: pref.enabled && armed && snooze_active,
    }
}

/// The reminder state machine over the vault's persisted preferences
/// (see the module's [reminder contract](self)). All transitions read,
/// mutate, and persist through [`DbPool`] — the single writer seam the
/// scheduler also uses — so every state change survives restart.
pub struct ReminderCore {
    pool: Arc<DbPool>,
    config: ReminderConfig,
}

impl ReminderCore {
    pub fn new(pool: Arc<DbPool>, config: ReminderConfig) -> Self {
        Self { pool, config }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.config.clock)()
    }

    /// Read one preference's status (None when the source never opted
    /// in). Due-ness is derived, never stored.
    pub fn status(&self, source: &str) -> Result<Option<ReminderStatus>, String> {
        self.pool
            .with_read(|s| s.reminder_preference(source))
            .map(|pref| pref.map(|p| evaluate(&p, self.now())))
            .map_err(|e| e.to_string())
    }

    /// Every source whose reminder is due right now — what a launch
    /// (or any later evaluation) presents. Sources closed over several
    /// missed deadlines appear once: a single instant cannot queue.
    pub fn due(&self) -> Result<Vec<ReminderStatus>, String> {
        let now = self.now();
        self.pool
            .with_read(|s| s.reminder_preferences())
            .map_err(|e| e.to_string())
            .map(|prefs| {
                prefs
                    .iter()
                    .map(|p| evaluate(p, now))
                    .filter(|s| s.due)
                    .collect()
            })
    }

    /// Opt a source in (or re-arm it): the interval is clamped to the
    /// configured bounds and the single deadline is set one full
    /// interval out — enabling is not "remind now"; a fresh arm never
    /// fires for time the reminder was off. Any snooze clears.
    pub fn enable(&self, source: &str, interval_ms: u64) -> Result<ReminderStatus, String> {
        let interval_ms =
            interval_ms.clamp(self.config.min_interval_ms, self.config.max_interval_ms);
        let now = self.now();
        let pref = ReminderPreference {
            source_instance_id: source.to_string(),
            enabled: true,
            interval_ms,
            next_due_at: Some(now + Duration::milliseconds(interval_ms as i64)),
            snoozed_until: None,
        };
        let written = self
            .pool
            .submit_write(move |s| s.set_reminder_preference(pref))
            .map_err(|e| e.to_string())?;
        Ok(evaluate(&written, now))
    }

    /// Opt a source out. The scheduling state is kept (not deleted) so
    /// the preference's history stays inspectable, but a disabled row
    /// is never due.
    pub fn disable(&self, source: &str) -> Result<ReminderStatus, String> {
        self.mutate(source, "disable", |mut pref, _| {
            pref.enabled = false;
            pref
        })
    }

    /// Defer a due reminder by `for_ms`: `snoozed_until = now + for_ms`.
    /// The schedule itself does not move — when the snooze passes, the
    /// reminder is simply due again.
    pub fn snooze(&self, source: &str, for_ms: u64) -> Result<ReminderStatus, String> {
        self.mutate(source, "snooze", move |mut pref, now| {
            pref.snoozed_until = Some(now + Duration::milliseconds(for_ms as i64));
            pref
        })
    }

    /// Acknowledge a due reminder: `next_due_at = now + interval`, any
    /// snooze clears. One dismissal buys one full interval of quiet.
    pub fn dismiss(&self, source: &str) -> Result<ReminderStatus, String> {
        self.mutate(source, "dismiss", |mut pref, now| {
            pref.next_due_at = Some(now + Duration::milliseconds(pref.interval_ms as i64));
            pref.snoozed_until = None;
            pref
        })
    }

    /// Read-modify-write one preference through the pool's single
    /// writer. The mutation sees the current clock time; a missing
    /// preference is an error (snoozing or dismissing a reminder that
    /// was never enabled is a caller bug, not a state change).
    fn mutate(
        &self,
        source: &str,
        what: &str,
        f: impl FnOnce(ReminderPreference, DateTime<Utc>) -> ReminderPreference + Send + 'static,
    ) -> Result<ReminderStatus, String> {
        let now = self.now();
        let existing = self
            .pool
            .with_read(|s| s.reminder_preference(source))
            .map_err(|e| e.to_string())?;
        let pref = existing.ok_or_else(|| {
            format!("cannot {what}: no reminder preference for source '{source}'")
        })?;
        let pref = f(pref, now);
        let written = self
            .pool
            .submit_write(move |s| s.set_reminder_preference(pref))
            .map_err(|e| e.to_string())?;
        Ok(evaluate(&written, now))
    }
}
