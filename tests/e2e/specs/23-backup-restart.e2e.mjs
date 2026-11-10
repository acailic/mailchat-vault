// Path: the U05 restart UX the Storage & backup panel renders at boot —
// driven on the live product:
//   - The persisted last-backup line renders after a finished backup and
//     honestly flags a container deleted since ("the file is no longer at
//     that path"), recomputed live and still honest after a restart.
//   - An interrupted switch (a `switching` journal planted over a
//     completed switch, simulating a crash between the journal write and
//     phase B) resolved by startup recovery: the restarted app rolls back
//     a completed-looking switch and the origin line says so — "A switch
//     started at T was interrupted and rolled back at startup — the vault
//     from before it is active." — instead of the false "Active vault:
//     switched in from the backup ..." claim.
//   - Storage & backup leads the Settings view (first section).
//
// The backup/restore commits go through the real IPC commands inside the
// live webview (the native save dialog is not WebDriver-reachable — the
// destination is passed exactly as the UI would pass it after the pick);
// the journal manipulation writes the persisted journal contract directly
// while the app is closed. Everything else runs through the real app
// and UI.
//
// The journal scenario deliberately exercises the U05 fix under review:
// the rolled-back outcome carries the resolved journal's created_at, so
// the origin line's recovery word applies to this journal — never a stale
// outcome from a later switch, never a silent degrade to "none".

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtemp, rm, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "vault-restart";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

function saveScreenshot(browser, filename) {
  if (!evidenceDir) return Promise.resolve(null);
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  return browser.saveScreenshot(file);
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

  let firstDataDir = "";
  let journalPath = "";
  const backupDir = await mkdtemp(path.join(tmpdir(), "mailchat-e2e-restart-"));
  const backupPath = path.join(backupDir, "vault.mcbackup");
  const restorePath = path.join(backupDir, "restore.mcbackup");
  try {
    // ---------------------------------------------------------------
    // Session 1: back the vault up, see the persisted line, delete the
    // container, see the honest flag (recomputed on the next render).
    // ---------------------------------------------------------------
    let recordCreatedAt = "";
    await withSession(
      {},
      async (browser, { dataDir }) => {
        firstDataDir = dataDir;
        journalPath = path.join(
          dataDir, "com.acailic.mailchatvault-recovery", "switch-journal.json",
        );
        await ui.waitForApp(browser);

        const imported = await ui.importMbox(browser, ui.archiveMbox);
        assert.ok(imported.inserted > 0, `the mbox import finished: ${JSON.stringify(imported)}`);

        log("      [vault-restart] submitting backup_create");
        const { id } = await ui.invokeCommand(browser, "job_submit", {
          kind: "backup_create",
          params: { dest: backupPath },
        });
        const job = await ui.waitForJob(browser, id);
        const summary = job.state.result ?? {};
        assert.equal(summary.path, backupPath, "the job reports the destination it wrote");
        assert.ok(existsSync(backupPath), "the container exists on disk");
        recordCreatedAt = summary.manifest.created_at;

        // Storage & backup leads Settings: #backup is the view's first
        // section, before the provider controls.
        const sectionOrder = await browser.execute(() =>
          Array.from(document.querySelectorAll("#view-settings > section"), (s) => s.id),
        );
        assert.equal(
          sectionOrder[0],
          "backup",
          `Storage & backup is the first Settings section: ${JSON.stringify(sectionOrder)}`,
        );

        // The persisted line renders in the panel.
        await openBackupPanel(browser, ui);
        await ui.waitForText(
          browser,
          "#backup-status",
          (t) => t.includes("Last verified backup:") && t.includes(backupPath),
          { timeoutMsg: "the last-backup line never rendered the finished backup" },
        );
        await saveScreenshot(browser, "23-backup-line.png");

        // Delete the container out from under the record, then re-enter
        // Settings — the line re-reads and flags the missing file instead
        // of promising it.
        await unlink(backupPath);
        await ui.openView(browser, "workspace");
        await ui.openView(browser, "settings");
        await ui.waitForText(
          browser,
          "#backup-status",
          (t) => t.includes("the file is no longer at that path"),
          { timeoutMsg: "the line never flagged the deleted container" },
        );
        await saveScreenshot(browser, "23-backup-missing.png");
      },
      { keepData: true },
    );

    // ---------------------------------------------------------------
    // Session 2: the record survives the restart (still honest), the
    // archive grows, a staged restore mints the completed journal and
    // previous-vault slot for the interruption simulation.
    // ---------------------------------------------------------------
    await withSession({ dataDir: firstDataDir }, async (browser) => {
      log("      [vault-restart] session 2 boot");
      await ui.waitForApp(browser);

      // The record rode vault.db across the restart; still_present is
      // recomputed (the container is still gone).
      await openBackupPanel(browser, ui);
      await ui.waitForText(
        browser,
        "#backup-status",
        (t) => t.includes("Last verified backup:") && t.includes("no longer at that path"),
        { timeoutMsg: "the missing-container line did not survive the restart" },
      );
      await saveScreenshot(browser, "23-backup-missing-restart.png");

      // Grow the archive beyond the backup point, then a second
      // container to restore from.
      const second = await ui.importMbox(
        browser,
        path.join(ui.fixturesDir, "mail-attachments.mbox"),
      );
      assert.ok(second.inserted > 0, `the second import finished: ${JSON.stringify(second)}`);

      const { id } = await ui.invokeCommand(browser, "job_submit", {
        kind: "backup_create",
        params: { dest: restorePath },
      });
      await ui.waitForJob(browser, id);
      assert.ok(existsSync(restorePath), "the second container exists on disk");

      // Staged validated preview → commit: the replaced (larger) vault
      // lands in a previous-vault slot; the journal completes.
      const { id: previewJob } = await ui.invokeCommand(browser, "job_submit", {
        kind: "restore_preview",
        params: { archive: restorePath },
      });
      const previewJobRecord = await ui.waitForJob(browser, previewJob);
      const previewResult = previewJobRecord.state.result ?? {};
      const previewId = previewResult.preview_id;
      assert.ok(previewId, `the staged preview carries its id: ${JSON.stringify(previewResult)}`);
      const committed = await ui.invokeCommand(browser, "restore_commit", {
        previewId,
      });
      assert.ok(committed.previous_slot, "the commit names the previous-vault slot");

      const status = await ui.invokeCommand(browser, "recovery_status");
      assert.equal(status.journal_state, "completed", "the switch journal completed");
      assert.equal(status.previous.length, 1, "exactly one previous-vault slot is recorded");
      assert.equal(
        status.last_switch.recovery,
        "none",
        "the switch completed in-session: recovery word none",
      );
      await saveScreenshot(browser, "23-restore-committed.png");
    }, { keepData: true });

    // ---------------------------------------------------------------
    // Session 3: plant the interruption — rewrite the persisted journal
    // (a contract file) from `completed` back to `switching`, exactly
    // the state a crash between the journal write and phase B leaves —
    // then restart. Startup recovery must roll back to the pre-switch
    // vault and the origin line must say so.
    // ---------------------------------------------------------------
    let plantedCreatedAt = "";
    {
      const journal = JSON.parse(readFileSync(journalPath, "utf8"));
      assert.equal(journal.state, "completed", "the journal reads completed before the plant");
      plantedCreatedAt = journal.created_at;
      journal.state = "switching";
      writeFileSync(journalPath, JSON.stringify(journal));
      log("      [vault-restart] journal planted as switching");
    }

    await withSession({ dataDir: firstDataDir }, async (browser, { dataDir }) => {
      log("      [vault-restart] session 3 boot (startup recovery)");
      await ui.waitForApp(browser);

      // The rollback consumed the slot: the pre-switch (larger) vault is
      // active again — proven by the database directly.
      if (sqlite) {
        const rows = await sqlite.query(
          vaultDbPath(dataDir),
          "SELECT COUNT(*) AS n FROM archive_messages",
        );
        assert.ok(rows[0].n > 8, `the pre-switch vault is active again: ${rows[0].n} messages`);
      }

      const status = await ui.invokeCommand(browser, "recovery_status");
      assert.equal(status.journal_state, "completed", "startup recovery resolved the journal");
      assert.equal(status.previous.length, 0, "the rollback consumed the slot");
      assert.equal(
        status.last_switch.recovery,
        "rolled_back",
        "the origin's recovery word names this journal's rollback",
      );
      assert.equal(
        status.last_switch.since,
        plantedCreatedAt,
        "the origin's since is the planted journal's created_at",
      );

      // The UI line: rolled back at startup, NOT the false restored claim.
      await openBackupPanel(browser, ui);
      await ui.waitForText(
        browser,
        "#vault-origin",
        (t) =>
          t.startsWith("A switch started at") &&
          t.includes("rolled back at startup") &&
          t.includes("the vault from before it is active"),
        { timeoutMsg: "the origin line never said the rollback out loud" },
      );
      await saveScreenshot(browser, "23-rolled-back-origin.png");
    }, { keepData: true });
  } finally {
    await rm(backupDir, { recursive: true, force: true });
  }
}
