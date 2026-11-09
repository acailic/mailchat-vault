// Path: pozadinski poslovi — the v2 background-jobs surface an end user
// sees: the jobs panel renders the live embed job through the v2 protocol
// (submit → started → progress → succeeded events), the embedding button
// tracks its own job (disabled while active), the embedding actually
// landed in the database, and the panel recovers its records after a
// webview reload (the `job_events_since(0)` boot-recovery path).

import { mkdirSync } from "node:fs";

export const name = "background-jobs";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) {
    return null;
  }
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

/** Jobs-panel list entries as text. */
async function jobLines(browser) {
  return browser.$$("#jobs-list li").map((li) => li.getText());
}

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    // --- 1. fresh boot: the jobs panel announces its empty state ---------
    const stats = await ui.waitForApp(browser);
    assert.equal(stats, "empty archive", "fresh install stats label");
    await ui.openView(browser, "activity");
    await ui.waitForText(
      browser,
      "#jobs-status",
      "No background jobs yet — imports, IMAP syncs and embedding run here.",
    );
    await saveScreenshot(browser, "12-jobs-panel-empty.png");

    // --- 2. import a mbox, then run the embed job through the v2 panel ---
    await ui.importMbox(browser, ui.archiveMbox);
    await ui.openView(browser, "workspace");
    const embedButton = await browser.$("#embed-btn");
    await embedButton.scrollIntoView({ block: "center" });
    await embedButton.click();

    // The job finishes fast (hash embedder, 8 messages); wait for the
    // terminal event to reach the panel header (Activity view). The
    // harness's mbox import is the other finished entry.
    await ui.openView(browser, "activity");
    await ui.waitForText(browser, "#jobs-status", "No active jobs (2 finished).");

    // The panel lists the finished jobs: the harness's mbox import (the
    // same v2 job the UI's confirm step would submit) plus the embed job.
    const lines = await jobLines(browser);
    assert.equal(lines.length, 2, "two finished job entries in the jobs panel");
    assert.ok(
      lines.some((line) => /^local embeddings — done \(8 embedded\)$/.test(line)),
      "embed job entry shows the result summary",
    );
    assert.ok(
      lines.some((line) => /^\.mbox import — done/.test(line)),
      "the mbox import job entry is listed too",
    );
    assert.ok(
      (await browser.$$("#jobs-list li.job.succeeded")).length === 2,
      "the entries carry the succeeded styling",
    );
    assert.equal(
      (await browser.$$("#jobs-list li.job.failed")).length,
      0,
      "no failed entries",
    );

    // The button tracked its own job: enabled again once nothing is active.
    assert.equal(
      await embedButton.isEnabled(),
      true,
      "embed button re-enabled after the job finished",
    );

    // The embedding actually landed: every archive message has a vector
    // for the shipped model (so a re-run's pending list — and therefore
    // the job's `remaining` — is 0).
    if (!sqlite) {
      ctx.log("      (sqlite3 CLI unavailable — skipping direct DB assertions)");
    } else {
      const db = vaultDbPath(dataDir);
      const rows = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM message_embeddings WHERE model = 'hash-ngram-v1'",
      );
      assert.equal(rows[0].n, 8, "8 embedding rows for the shipped model");
    }
    await saveScreenshot(browser, "12-jobs-panel-after-embed.png");

    // --- 3. webview reload: boot recovery rebuilds the panel -------------
    await browser.refresh();
    await ui.waitForApp(browser);
    await ui.openView(browser, "activity");
    await ui.waitForText(browser, "#jobs-status", "No active jobs (2 finished).");
    const reloaded = await jobLines(browser);
    assert.ok(
      reloaded.some((line) => /^local embeddings — done \(8 embedded\)$/.test(line)),
      "jobs panel restored from job_events_since(0) after reload",
    );
    await saveScreenshot(browser, "12-jobs-panel-after-reload.png");
  });
}
