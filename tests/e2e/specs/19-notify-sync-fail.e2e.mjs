// Path: the notification layer's "only a cycle that stored something new
// notifies" contract for the IMAP paths, at the live product surface:
//   - the v1 Sync button against a dead local port (implicit TLS never
//     completes against a plain TCP listener): the cycle fails with a
//     visible "Sync failed" status and no OS notification fires — a
//     failed cycle never counts;
//   - an `imap_sync_all` coordinator round over a shared helper shape
//     (see 14-imap-coordinator) with no stored credentials: the round job
//     SUCCEEDS with a `no_credentials` outcome (nothing stored), and
//     still no notification — a succeeded round that stored nothing
//     never notifies either.
//
// Like 18, this spec reads the captured DBus Notify calls from the
// wrapper's dbus-monitor capture (MAILCHAT_E2E_NOTIFY_LOG) — the real OS
// notification surface on Linux.

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, mkdir } from "node:fs/promises";

export const name = "notify-sync-fail";

const pexec = promisify(execFile);
const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;
const busLog = process.env.MAILCHAT_E2E_NOTIFY_LOG || null;

function requireBusLog(ctx) {
  if (!busLog) {
    ctx.skip("MAILCHAT_E2E_NOTIFY_LOG not set - no notification-bus capture to assert on");
  }
}

/** Count `member=Notify` DBus method calls captured so far. */
async function notifyCount() {
  const text = await readFile(busLog, "utf8").catch(() => "");
  return (text.match(/member=Notify/g) ?? []).length;
}

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return null;
  await mkdir(evidenceDir, { recursive: true });
  try {
    await pexec("scrot", [`${evidenceDir}/${filename}`]);
  } catch {
    // bonus
  }
  try {
    await browser.saveScreenshot(`${evidenceDir}/${filename.replace(".png", "-webview.png")}`);
  } catch {
    // ignore
  }
  return `${evidenceDir}/${filename}`;
}

/** Set a text input's value (WebKitWebDriver rejects setValue("")). */
async function setInput(browser, selector, value) {
  await browser.execute(
    (sel, val) => {
      const el = document.querySelector(sel);
      el.value = val;
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    selector,
    value,
  );
}

/** Invoke a Tauri command inside the live webview (real IPC, real app). */
async function ipc(browser, cmd, args = {}) {
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
    { timeout: 30000, timeoutMsg: `${cmd} never settled in the webview` },
  );
  const settled = await browser.execute(() => window.__e2eInvoke);
  if (settled.err !== undefined) {
    throw new Error(`${cmd} failed in webview: ${settled.err}`);
  }
  return settled.ok;
}

/** Poll a v2 job until it reaches a terminal state. */
async function awaitJob(browser, jobId, { timeoutMs = 60000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rec = await ipc(browser, "job_get", { id: jobId });
    const state = rec?.state?.state;
    if (state === "succeeded" || state === "failed" || state === "cancelled") {
      return rec;
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  throw new Error(`job ${jobId} never reached a terminal state within ${timeoutMs}ms`);
}

export async function run(ctx) {
  const { withSession, assert, ui, sqlite } = ctx;
  requireBusLog(ctx);

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    await ui.openSourcesSection(browser, "email");
    const before = await notifyCount();

    // ==============================================================
    // 1. Failed manual sync: dead local port -> "Sync failed", and no
    //    notification ever fires.
    // ==============================================================
    await setInput(browser, "#imap-host", "127.0.0.1");
    await setInput(browser, "#imap-port", "59993");
    await setInput(browser, "#imap-user", "u@h.invalid");
    await (await browser.$("#imap-pass")).setValue("not-a-real-password");
    await (await browser.$("#imap-sync-btn")).click();
    await ui.waitForText(browser, "#imap-status", "Sync failed", {
      timeout: 60000,
      timeoutMsg: "the sync against a dead port never surfaced its failure",
    });
    await saveScreenshot(browser, "19-sync-failed-no-notify.png");
    const afterFail = await notifyCount();
    assert.equal(afterFail, before, "a failed sync cycle never notifies");

    // ==============================================================
    // 2. Coordinator round over a configured source with no stored
    //    credentials: the round succeeds, nothing is stored, nothing
    //    notifies.
    // ==============================================================
    if (!sqlite) {
      throw new Error("sqlite3 CLI unavailable - this spec needs direct DB asserts");
    }
    const db = ctx.vaultDbPath(dataDir);
    const config = JSON.stringify({
      host: "127.0.0.1",
      port: 993,
      tls_mode: "implicit-tls",
      username: "credless@h.invalid",
      folders: ["INBOX"],
    });
    const configEsc = config.replace(/'/g, "''");
    await ctx.sqlite.query(db, "INSERT INTO source_instances (id, kind, label, connector_key, config_json, state, excluded) VALUES ('e2e-imap-credless', 'imap', 'imap:mail.test:993:credless@h.invalid', 'imap|127.0.0.1|993|implicit-tls|credless@h.invalid', '" + configEsc + "', 'connected', 0)");

    const round = await ipc(browser, "job_submit", { kind: "imap_sync_all", params: {} });
    assert.equal(typeof round.id, "number", "imap_sync_all submits as a v2 job");
    const rec = await awaitJob(browser, round.id);
    assert.equal(rec.state.state, "succeeded", "the credless round succeeds as a job");
    assert.ok(
      JSON.stringify(rec.state.result).includes("no_credentials"),
      "the round outcome is no_credentials (nothing stored)",
    );

    await ui.openView(browser, "activity");
    await ui.waitForText(browser, "#jobs-list", "IMAP sync (all sources) \u2014 done", {
      timeoutMsg: "the jobs panel never showed the finished round",
    });
    await browser.execute(() =>
      document.querySelector("#jobs-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "19-round-done-no-notify.png");

    const afterRound = await notifyCount();
    assert.equal(afterRound, before, "a succeeded round that stored nothing never notifies");
  });
}
