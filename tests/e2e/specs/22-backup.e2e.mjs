// Path: the M4.4 Storage & backup surface an end user sees — one versioned
// container of the whole vault, a validated staged restore that replaces the
// vault through the recoverable switch journal, and the previous-vault
// return path — driven on the live product:
//   - Create backup: the `backup_create` job writes one self-describing
//     container; the Activity panel lists it; no snapshot temp litter is
//     left beside the destination.
//   - Restore: inspect → staged validated preview → commit replaces the
//     vault; the replaced vault's predecessor lands whole in a
//     previous-vault slot; the restarted app opens the restored vault.
//   - Return to previous vault: the recovery panel renders the slot; the
//     backend return switches back; the restarted app opens the original
//     vault again.
//   - Fail-closed startup recovery: a planted unresolved `switching`
//     journal whose previous slot lacks a database makes the app refuse to
//     open the vault at all (no fresh empty vault.db appears, the journal
//     stays untouched for the user to resolve).
//
// The commits and returns here go through the real IPC commands
// (`restore_commit` / `return_to_previous_vault`) inside the live webview
// rather than the native-file-dialog button handlers — WebKit's scripted
// `window.confirm` dialog is not reachable through the WebDriver session —
// every other step (imports, panel rendering, Refresh clicks, restarts)
// is driven through the real UI. The scheduling invariants (concurrent
// switch serialization, marker contract) are proven in the src-tauri unit
// tests and vault-core's backup suite.

import { existsSync, readdirSync, mkdirSync } from "node:fs";
import { mkdtemp, rm, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "vault-backup";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

function saveScreenshot(browser, filename) {
  if (!evidenceDir) return Promise.resolve(null);
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  return browser.saveScreenshot(file);
}

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

/** Poll until the job reaches a terminal state, return its succeeded
 *  result (or throw on failure). Real job protocol over live IPC. */
async function waitForJob(browser, id, { timeout = 90_000 } = {}) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const record = await ipc(browser, "job_get", { id });
    const state = record?.state?.state;
    if (state === "succeeded") return record.state.result;
    if (state === "failed") {
      throw new Error(`job ${id} failed: ${JSON.stringify(record.state)}`);
    }
    if (state === "cancelled") throw new Error(`job ${id} was cancelled`);
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`job ${id} never finished within ${timeout}ms`);
}

/** Open the Settings view and bring the Storage & backup panel on screen. */
async function openBackupPanel(browser, ui) {
  await ui.openView(browser, "settings");
  await browser.execute(() =>
    document.querySelector("#backup")?.scrollIntoView({ block: "center" }),
  );
}

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath, sqlite, log } = ctx;
  if (!sqlite) {
    log("      (sqlite3 CLI unavailable — database-count assertions will be skipped)");
  }

  // One disposable destination for the backup container (outside every
  // session's data dir; the harness wipes the data dirs, not this one).
  const backupDir = await mkdtemp(path.join(tmpdir(), "mailchat-e2e-backup-"));
  const backupPath = path.join(backupDir, "vault.mcbackup");
  try {
    // ---------------------------------------------------------------
    // Session 1: build a vault, back it up, change the vault, restore.
    // ---------------------------------------------------------------
    let firstDataDir;
    let backupMessages = 0;
    let originalMessages = 0;
    let returnedSlotId = "";
    await withSession(
      {},
      async (browser, { dataDir }) => {
        firstDataDir = dataDir;
      await ui.waitForApp(browser);

      // 1. A known 8-message archive.
      const imported = await ui.importMbox(browser, ui.archiveMbox);
      assert.ok(imported.includes(" new, "), `the mbox import finished: ${imported}`);
      const db = vaultDbPath(dataDir);
      if (sqlite) {
        const rows = await sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
        backupMessages = rows[0].n;
        assert.equal(backupMessages, 8, "the fixture imports 8 messages");
      }

      await openBackupPanel(browser, ui);
      await saveScreenshot(browser, "22-backup-panel-before.png");

      // 2. Create backup through the real job pipeline. The native save
      //    dialog is not WebDriver-reachable, so the destination is passed
      //    exactly as the UI would pass it after the pick.
      log("      [vault-backup] submitting backup_create");
      const { id } = await ipc(browser, "job_submit", {
        kind: "backup_create",
        params: { dest: backupPath },
      });
      const summary = await waitForJob(browser, id);
      log("      [vault-backup] backup job finished");
      assert.equal(summary.path, backupPath, "the job reports the destination it wrote");
      assert.ok(summary.size > 0, "the container is non-empty");
      assert.ok(existsSync(backupPath), "the container exists on disk");
      assert.ok(
        Number.isInteger(summary.manifest.counts.messages) &&
          summary.manifest.counts.messages > 0,
        `the manifest counts the archived messages: ${JSON.stringify(summary.manifest.counts)}`,
      );
      if (sqlite) {
        assert.equal(
          summary.manifest.counts.messages,
          backupMessages,
          "the manifest's message count matches the live database",
        );
      }

      // No snapshot temp litter beside the destination (the backup_create
      // job must clean its working file on every exit path).
      const leftovers = readdirSync(backupDir).filter((n) => n.includes("-snapshot-"));
      assert.equal(leftovers.length, 0, `no snapshot temp files remain: ${leftovers}`);

      // The Activity panel lists the finished backup job through the real
      // job-event stream.
      log("      [vault-backup] litter checked; opening activity");
      await ui.openView(browser, "activity");
      log("      [vault-backup] activity open");
      await browser.waitUntil(
        async () => {
          const texts = await browser.execute(() =>
            Array.from(document.querySelectorAll("#jobs-list li"), (li) => li.textContent ?? ""),
          );
          return texts.some((t) => t.includes("backup_create"));
        },
        { timeout: 15_000, timeoutMsg: "the jobs panel never listed the backup job" },
      );
      log("      [vault-backup] jobs panel listed the backup");
      await saveScreenshot(browser, "22-backup-job-in-activity.png");

      // 3. Change the vault so the restore has something to undo.
      const second = await ui.importMbox(
        browser,
        path.join(ui.fixturesDir, "mail-attachments.mbox"),
      );
      assert.ok(second.includes(" new, "), `the second import finished: ${second}`);
      if (sqlite) {
        const rows = await sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
        originalMessages = rows[0].n;
        assert.ok(
          originalMessages > backupMessages,
          "the second import grew the archive beyond the backup point",
        );
      }

      // 4. Restore: inspect → staged validated preview → commit.
      const manifest = await ipc(browser, "backup_inspect", { archive: backupPath });
      assert.equal(
        manifest.counts.messages,
        summary.manifest.counts.messages,
        "inspect re-reads the same manifest counts",
      );

      const { id: previewJob } = await ipc(browser, "job_submit", {
        kind: "restore_preview",
        params: { archive: backupPath },
      });
      const preview = await waitForJob(browser, previewJob);
      assert.ok(preview.preview_id, "the staged preview carries its id");
      if (sqlite) {
        assert.equal(
          preview.manifest.counts.messages,
          backupMessages,
          "the validated preview counts the backup-era messages",
        );
      }

      // The staged preview exists on the recovery surface (asserted over
      // live IPC below); with no previous-vault slots yet, the panel
      // headline shows its empty state — the staged flow itself lives in
      // the restore panel inside the validating session.
      await openBackupPanel(browser, ui);
      const refresh = await browser.$("#recovery-refresh-btn");
      await refresh.scrollIntoView({ block: "center" });
      await refresh.click();
      await ui.waitForText(
        browser,
        "#recovery-line",
        (t) => t.includes("No previous-vault snapshots recorded yet"),
        { timeoutMsg: "the recovery headline never settled" },
      );
      const stagedStatus = await ipc(browser, "recovery_status");
      assert.equal(
        stagedStatus.staged_preview,
        true,
        "the staged preview is visible on the recovery surface",
      );
      await saveScreenshot(browser, "22-backup-staged-preview.png");

      const committed = await ipc(browser, "restore_commit", {
        previewId: preview.preview_id,
      });
      assert.equal(
        committed.sources_needing_reauth,
        0,
        "no IMAP accounts exist in this archive, so nothing needs reconnection",
      );
      assert.ok(committed.previous_slot, "the outcome names the previous-vault slot");

      // The active database is now the restored one: the later import is
      // gone, the backup-era count is back.
      if (sqlite) {
        const restoredRows = await sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
        assert.equal(
          restoredRows[0].n,
          backupMessages,
          "the replaced vault holds exactly the backup-era messages",
        );
      }
      const afterCommit = await ipc(browser, "recovery_status");
      assert.equal(afterCommit.journal_state, "completed", "the switch journal completed");
      assert.equal(
        afterCommit.previous.length,
        1,
        "exactly one previous-vault slot is recorded after the restore",
      );

      // Refreshing the panel after the switch: nothing staged remains and
      // the completed slot is listed.
      await refresh.click();
      await ui.waitForText(
        browser,
        "#recovery-line",
        (t) => t.includes("previous-vault snapshot") && t.includes("kept"),
        { timeoutMsg: "the recovery headline never showed the completed slot list" },
      );
    },
    { keepData: true },
    );

    // ---------------------------------------------------------------
    // Session 2: the restarted app opens the RESTORED vault; the
    // recovery panel renders the slot; returning switches back.
    // ---------------------------------------------------------------
    await withSession({ dataDir: firstDataDir }, async (browser) => {
      log("      [vault-backup] session 2 boot");
      await ui.waitForApp(browser);
      log("      [vault-backup] session 2 booted");
      const stats = await browser.execute(
        () => document.querySelector("#stats")?.textContent ?? "",
      );
      if (sqlite) {
        assert.ok(
          stats.includes(`${backupMessages} messages`),
          `the restarted app opened the restored vault (stats: ${stats})`,
        );
      }

      await openBackupPanel(browser, ui);
      const refresh = await browser.$("#recovery-refresh-btn");
      await refresh.scrollIntoView({ block: "center" });
      await refresh.click();
      await ui.waitForText(
        browser,
        "#recovery-slots",
        (t) => t.includes("·"),
        { timeoutMsg: "the recovery panel never rendered the previous-vault slot" },
      );
      await saveScreenshot(browser, "22-backup-restored-vault-slot.png");

      // Return to the previous vault through the real switch path.
      const slots = await ipc(browser, "recovery_status");
      assert.equal(slots.previous.length, 1, "the restored boot sees one slot");
      returnedSlotId = slots.previous[0].id;
      const outcome = await ipc(browser, "return_to_previous_vault", {
        slotId: slots.previous[0].id,
      });
      log("      [vault-backup] session 2 return done");
      assert.ok(outcome.previous_slot, "the return names the slot it kept");
    }, { keepData: true });

    // ---------------------------------------------------------------
    // Session 3: the app opens the original vault again.
    // ---------------------------------------------------------------
    await withSession({ dataDir: firstDataDir }, async (browser, { dataDir }) => {
      log("      [vault-backup] session 3 boot");
      await ui.waitForApp(browser);
      log("      [vault-backup] session 3 booted");
      if (sqlite) {
        const rows = await sqlite.query(
          vaultDbPath(dataDir),
          "SELECT COUNT(*) AS n FROM archive_messages",
        );
        assert.equal(
          rows[0].n,
          originalMessages,
          "the returned-to vault holds every message from before the restore",
        );
      }
      const after = await ipc(browser, "recovery_status");
      assert.equal(
        after.previous.length,
        1,
        "the return consumed the chosen slot and kept the restored vault as the one recorded slot",
      );
      assert.ok(
        after.previous[0].id !== returnedSlotId,
        "the remaining slot is the replaced restored vault, not the consumed one",
      );
    }, { keepData: true });

    // ---------------------------------------------------------------
    // Fail-closed startup recovery, driven directly against the real app
    // binary (the WebDriver session cannot observe an app that refuses
    // to start — session creation wedges on the dead process). A planted
    // `switching` journal whose previous slot holds no vault.db must
    // abort the app start before any pool is spawned: the process exits
    // with the refusal, no fresh empty vault.db appears, and the journal
    // stays for the user to resolve.
    // ---------------------------------------------------------------
    log("      [vault-backup] fail-closed boot attempt (direct spawn)");
    const brokenDir = await mkdtemp(path.join(tmpdir(), "mailchat-e2e-broken-"));
    const appDir = path.join(brokenDir, "com.acailic.mailchatvault");
    // The switch journal lives in the vault root's -recovery sibling.
    const recoveryDir = path.join(brokenDir, "com.acailic.mailchatvault-recovery");
    await mkdir(recoveryDir, { recursive: true });
    const journalPath = path.join(recoveryDir, "switch-journal.json");
    await writeFile(
      journalPath,
      JSON.stringify({
        version: 1,
        state: "switching",
        created_at: "2026-11-07T00:00:00Z",
        archive: null,
        previous: path.join(recoveryDir, "previous-empty"),
        source: path.join(recoveryDir, "staging-missing"),
      }),
    );
    try {
      const { spawn } = await import("node:child_process");
      const { appBinary } = await import("../harness/app.mjs");
      const stderr = [];
      const child = spawn(appBinary(), [], {
        env: {
          ...process.env,
          XDG_DATA_HOME: brokenDir,
          WEBKIT_DISABLE_COMPOSITING_MODE: "1",
          NO_AT_BRIDGE: "1",
        },
        stdio: ["ignore", "ignore", "pipe"],
      });
      child.stderr.on("data", (chunk) => stderr.push(String(chunk)));
      const exitCode = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("refusing app never exited")), 60_000);
        child.on("exit", (code) => {
          clearTimeout(timer);
          resolve(code);
        });
        child.on("error", reject);
      });
      const text = stderr.join("");
      assert.ok(
        text.includes("an interrupted vault switch could not be resolved"),
        `the app names the unresolved switch as the refusal: ${text.slice(-400)}`,
      );
      assert.ok(
        text.includes("the vault will not be opened in this state"),
        "the app says the vault stays closed in this state",
      );
      assert.ok(exitCode !== 0, `the app exits non-zero (got ${exitCode})`);
      assert.equal(
        existsSync(path.join(appDir, "vault.db")),
        false,
        "no fresh empty vault.db was spawned over the broken state",
      );
      assert.ok(
        existsSync(journalPath),
        "the unresolved journal stays on disk for the user to resolve",
      );
    } finally {
      await rm(brokenDir, { recursive: true, force: true });
    }
  } finally {
    await rm(backupDir, { recursive: true, force: true });
  }
}
