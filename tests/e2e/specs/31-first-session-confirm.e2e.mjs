// Path: GP03 — the first-session confirmation surfaces on the live app:
// the preview's naive-stamp fields, a confirmed-timezone import run, and
// the explicit destination source. The native picker stays beyond
// WebDriver (the documented limitation, spec 25): this spec drives the
// same preview → confirm → job contract the picker hands its result to.
// Local note: authored with the WebKitGTK host limitation in mind — it
// runs wherever the other import specs run; no local run is claimed.

import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "first-session-confirm";

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath } = ctx;

  await withSession(
    {},
    async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // --- a naive-date Telegram export reports its assumption ---------
    const dir = mkdtempSync(path.join(tmpdir(), "mc31-"));
    const telegramPath = path.join(dir, "result.json");
    writeFileSync(
      telegramPath,
      JSON.stringify({
        name: "Arhiva",
        id: 42,
        messages: [
          { id: 1, type: "message", date: "2026-11-01T10:00:00", from: "Marko", text: "naive stamp" },
          { id: 2, type: "message", date: "2026-11-01T11:00:00", from: "Ana", text: "another" },
        ],
      }),
    );
    const preview = await ui.invokeCommand(browser, "preview_import", {
      path: telegramPath,
    });
    assert.equal(preview.kind, "telegram");
    assert.equal(
      preview.naive_timestamps,
      2,
      "the confirm card's data: both stamps are naive",
    );
    assert.equal(preview.assumed_zone, "UTC", "the naive fallback assumes UTC");

    // --- mbox carries explicit offsets: nothing to confirm ------------
    const mbox = await ui.invokeCommand(browser, "preview_import", {
      path: ui.archiveMbox,
    });
    assert.equal(mbox.naive_timestamps, 0, "RFC 5322 dates are not naive");
    assert.equal(mbox.assumed_zone, null);

    // --- confirm: the zone correction and the chosen source -----------
    // The same params the preview card's Import button submits after a
    // corrected zone and a typed destination source.
    const { id } = await ui.invokeCommand(browser, "job_submit", {
      kind: "import_chat_export",
      params: {
        path: telegramPath,
        extract_media: false,
        confirmed_timezone: "Europe/Berlin",
        source: "Family chats",
      },
    });
    const job = await ui.waitForJob(browser, id);
    assert.equal(job.state.state, "succeeded", `import succeeded: ${job.state.error?.message ?? ""}`);
    assert.deepEqual(job.state.result.summary, {
      source: "Family chats",
      seen: 2,
      inserted: 2,
      skipped: 0,
      failed: 0,
    }, "the run stores under the chosen destination source");

    // --- the confirmed zone reached the rows ---------------------------
    if (sqlite) {
      const db = vaultDbPath(dataDir);
      const rows = await sqlite.query(
        db,
        `SELECT m.sent_instant, m.timezone, m.time_confidence, s.legacy_namespace AS source
         FROM archive_messages m
         JOIN source_instances s ON s.id = m.source_instance_id
         ORDER BY m.id`,
      );
      assert.equal(rows.length, 2);
      for (const row of rows) {
        assert.equal(row.timezone, "Europe/Berlin", "the confirmed zone is recorded");
        assert.equal(row.time_confidence, "user_confirmed", "the confirmed label is recorded");
        // The user-chosen namespace ("Family chats") is pinned by the
        // job-summary assert above; after the source-instances
        // normalization the rows' instance carries the kind label
        // ("telegram") and the namespace mapping is the summary's
        // contract, not the row's source column.
      }
      // 2026-11-01 is CET (UTC+1): the 10:00 wall-time resolves to 09:00Z.
      assert.equal(rows[0].sent_instant, "2026-11-01T09:00:00Z");
    }

    // --- re-import with the same params never duplicates ---------------
    const again = await ui.invokeCommand(browser, "job_submit", {
      kind: "import_chat_export",
      params: {
        path: telegramPath,
        extract_media: false,
        confirmed_timezone: "Europe/Berlin",
        source: "Family chats",
      },
    });
    const retryJob = await ui.waitForJob(browser, again.id);
    assert.equal(retryJob.state.state, "succeeded");
    assert.deepEqual(retryJob.state.result.summary, {
      source: "Family chats",
      seen: 2,
      inserted: 0,
      skipped: 2,
      failed: 0,
    }, "a re-run in the same namespace skips everything");
  });
}
