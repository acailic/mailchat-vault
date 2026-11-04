// Path: the M2.4 periodic-sync scheduler surface an end user sees —
// `sync_schedule_get` / `sync_schedule_set` through the live IPC bridge,
// the auto-sync toggle in the IMAP section, persistence of the opt-in in
// the vault database, and the fresh-start behavior across an app restart.
//
// The scheduling invariants themselves (rounds never overlap even when a
// round outlasts the interval; sleep/offline gaps collapse to one
// catch-up round) are proven with real threads and injected clocks in
// `crates/vault-core/tests/scheduler.rs`; the product's 15-minute
// default cadence is far too slow to observe in an e2e spec. What this
// spec proves on the live product:
//   - the schedule is opt-in: a fresh install reports off at 15 minutes;
//   - the toggle enables it through the real IPC and the UI renders the
//     armed state ("next round in ~15 min", not an immediate round);
//   - the interval is validated (1..=1440 minutes) with a clear error;
//   - the opt-in persists in the vault database (sync_schedule row) and
//     survives an app restart — where the scheduler re-arms one full
//     interval out instead of firing an immediate round.

export const name = "sync-scheduler";

/** Invoke a Tauri command inside the live webview (real IPC, real app). */
async function ipc(browser, cmd, args = {}) {
  const settled = await ipcSettled(browser, cmd, args);
  if (settled.err !== undefined) {
    throw new Error(`${cmd} failed in webview: ${settled.err}`);
  }
  return settled.ok;
}

async function ipcSettled(browser, cmd, args) {
  await browser.execute(
    (command, payload) => {
      window.__e2eInvoke = null;
      window.__TAURI_INTERNALS__.invoke(command, payload)
        .then((value) => {
          window.__e2eInvoke = { ok: value };
        })
        .catch((error) => {
          window.__e2eInvoke = { err: String(error) };
        });
    },
    cmd,
    args,
  );
  await browser.waitUntil(
    async () => (await browser.execute(() => window.__e2eInvoke)) !== null,
    { timeout: 20000, timeoutMsg: `${cmd} never settled in the webview` },
  );
  const settled = await browser.execute(() => window.__e2eInvoke);
  await browser.execute(() => {
    delete window.__e2eInvoke;
  });
  return settled;
}

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  let firstDataDir;
  await withSession({}, async (browser, { dataDir }) => {
    firstDataDir = dataDir;
    await ui.waitForApp(browser);
    await ui.openView(browser, "sources");
    const db = vaultDbPath(dataDir);

    // 1. Opt-in default: off, 15-minute interval, nothing scheduled.
    const fresh = await ipc(browser, "sync_schedule_get");
    assert.equal(fresh.enabled, false, "a fresh install schedules nothing");
    assert.equal(fresh.interval_minutes, 15, "the default interval is 15 minutes");
    assert.equal(fresh.active_job, null, "no scheduled round is active");
    assert.equal(fresh.next_due_in_secs, null, "nothing is due while disabled");

    // The UI mirror: toggle unchecked, off-status rendered.
    const offText = await ui.waitForText(
      browser,
      "#auto-sync-status",
      (t) => t.includes("Auto-sync off"),
      { timeoutMsg: "the auto-sync status never rendered the off state" },
    );
    assert.ok(offText.length > 0, "off-state text is non-empty");
    const toggleState = await browser.execute(() => ({
      checked: document.querySelector("#auto-sync-toggle")?.checked,
      interval: document.querySelector("#auto-sync-interval")?.value,
    }));
    assert.equal(toggleState.checked, false, "the toggle starts unchecked");
    assert.equal(toggleState.interval, "15", "the interval select starts at 15");

    // 2. Interval validation through the live IPC.
    const tooSmall = await ipcSettled(browser, "sync_schedule_set", {
      enabled: false,
      intervalMinutes: 0,
    });
    assert.ok(
      String(tooSmall.err ?? "").includes("interval_minutes"),
      `0 minutes is refused with a clear error, got: ${tooSmall.err}`,
    );

    // A command issued outside the UI must update the interval through
    // scheduler-status alone; the command response is not rendered here.
    await ipc(browser, "sync_schedule_set", { enabled: true, intervalMinutes: 60 });
    await ui.waitForText(browser, "#auto-sync-status", (t) => t.includes("Auto-sync on"));
    assert.equal(
      await browser.execute(() => document.querySelector("#auto-sync-interval").value),
      "60",
      "scheduler events use the same minute-based view as command responses",
    );
    await ipc(browser, "sync_schedule_set", { enabled: false, intervalMinutes: 15 });
    await ui.waitForText(browser, "#auto-sync-status", (t) => t.includes("Auto-sync off"));

    // 3. Opt in through the real UI: check the toggle at 30 minutes.
    await browser.execute(() => {
      document.querySelector("#auto-sync-interval").value = "30";
      document.querySelector("#auto-sync-interval").dispatchEvent(new Event("change", { bubbles: true }));
      const toggle = document.querySelector("#auto-sync-toggle");
      toggle.checked = true;
      toggle.dispatchEvent(new Event("change", { bubbles: true }));
    });
    const on = await ui.waitForText(
      browser,
      "#auto-sync-status",
      (t) => t.includes("Auto-sync on"),
      { timeoutMsg: "the auto-sync status never rendered the on state" },
    );
    assert.ok(on.includes("~30 min"), `the armed countdown renders, got: ${on}`);

    const view = await ipc(browser, "sync_schedule_get");
    assert.equal(view.enabled, true, "the schedule reports enabled");
    assert.equal(view.interval_minutes, 30, "the chosen interval is stored");
    // Enabling arms one full interval out — never an immediate round.
    assert.equal(view.rounds_submitted, 0, "enabling did not submit a round");
    assert.equal(view.next_due_in_secs, 1800, "the first round is one interval out");

    // 4. The opt-in is durable: the singleton row in the vault database
    // (direct reads follow the suite contract: skipped when the sqlite3
    // CLI is unavailable, on the runner announcement).
    if (!ctx.sqlite) {
      ctx.log(
        "      (sqlite3 CLI unavailable — skipping direct DB assertions)",
      );
    } else {
      const rows = await ctx.sqlite.query(
        db,
        "SELECT enabled, interval_ms FROM sync_schedule WHERE id = 1",
      );
      assert.equal(rows.length, 1, "one schedule row");
      assert.equal(
        Number(rows[0].enabled),
        1,
        "the schedule persists enabled in the vault database",
      );
      assert.equal(
        Number(rows[0].interval_ms),
        1_800_000,
        "the interval persists in the vault database",
      );
    }
  },
    { keepData: true },
  );

  // 5. Restart over the same data dir: the scheduler re-arms from the
  // vault — still enabled, still 30 minutes, and no immediate round.
  await withSession({ dataDir: firstDataDir }, async (browser) => {
    await ui.waitForApp(browser);
    await ui.openView(browser, "sources");
    const after = await ipc(browser, "sync_schedule_get");
    assert.equal(after.enabled, true, "the opt-in survives the restart");
    assert.equal(after.interval_minutes, 30, "the interval survives the restart");
    assert.equal(
      after.rounds_submitted,
      0,
      "a fresh start arms one interval out — no immediate catch-up round",
    );
    assert.equal(after.next_due_in_secs, 1800, "the next round is one interval out");

    const text = await ui.waitForText(
      browser,
      "#auto-sync-status",
      (t) => t.includes("Auto-sync on"),
      { timeoutMsg: "the restarted app never re-rendered the on state" },
    );
    assert.ok(text.includes("~30 min"), `the restarted UI shows the countdown, got: ${text}`);

    // Opt back out through the IPC and see the UI follow via the event.
    await ipc(browser, "sync_schedule_set", { enabled: false, intervalMinutes: null });
    await ui.waitForText(
      browser,
      "#auto-sync-status",
      (t) => t.includes("Auto-sync off"),
      { timeoutMsg: "disabling never re-rendered the off state" },
    );
    const off = await ipc(browser, "sync_schedule_get");
    assert.equal(off.enabled, false, "disabling persists");
    assert.equal(off.next_due_in_secs, null, "nothing is due once disabled");
  });
}
