// Path: import a JSON Lines (.jsonl) export end-to-end through the
// running app — the export format's import counterpart, closing the
// round-trip at the level of single messages. Like the signal-import
// spec, the native file chooser cannot be automated over WebDriver, so
// this spec submits the very same import_chat_export job the confirm
// button submits, exercises the bounded preview over real IPC, then
// reloads the app so the normal boot path renders the archive. The
// rows keep their original telegram source identity (never re-namespaced
// under `jsonl`), a malformed line becomes a counted failure, and a
// re-import dedups — the same contract the unit suites assert, proven
// on the real binary.
export const name = "jsonl-import";

import { writeFile } from "node:fs/promises";
import path from "node:path";

/** One exported row, exactly the shape the JSON Lines export writes. */
function row(id, externalId, author, sentAt, body) {
  return JSON.stringify({
    id,
    source: "telegram",
    external_id: externalId,
    channel: "chat",
    chat_id: "Arhiva",
    author,
    sent_at: sentAt,
    body,
    raw_json: "{}",
  });
}

const JSONL = [
  row(11, "42/7", "Marko", "2026-11-01T10:00:00Z", "zdravo ekipa iz jsonl izvoza"),
  row(12, "42/8", "Jelena", "2026-11-01T10:01:00Z", "vidimo se na kraju"),
  "{not json}",
  row(13, "42/9", "Ana", "2026-11-01T10:02:00Z", "poslednja poruka"),
].join("\n");

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    const jsonlPath = path.join(dataDir, "subset.jsonl");
    await writeFile(jsonlPath, JSONL);

    // --- the bounded preview knows the format before anything is stored --
    const preview = await ui.invokeCommand(browser, "preview_import", {
      path: jsonlPath,
    });
    assert.equal(preview.kind, "jsonl");
    assert.equal(preview.source, "jsonl");
    assert.equal(preview.messages_seen, 3, "three well-formed rows seen");
    assert.equal(preview.parse_failures, 1, "the malformed line counted");
    assert.equal(preview.scan_complete, true);

    // --- import via the UI's confirm path (job_submit) --------------------
    const summary = await ui.importChatExport(browser, jsonlPath, {
      extractMedia: false,
    });
    assert.ok(summary.ok, `import job succeeded: ${summary.error ?? ""}`);
    const s = summary.value.summary;
    assert.ok(
      s.source === "jsonl" && s.seen === 4 && s.inserted === 3 && s.skipped === 0 && s.failed === 1,
      "import summary: 3 rows archived under their own sources, 1 broken line counted " +
        `(got source=${s.source} seen=${s.seen} inserted=${s.inserted} skipped=${s.skipped} failed=${s.failed})`,
    );

    // --- reload: rows landed under their original namespace, not `jsonl` --
    await browser.execute(() => location.reload());
    const statsText = await ui.waitForApp(browser);
    assert.equal(statsText, "3 messages (telegram: 3)", "stats label keeps the row source");

    // --- the imported texts are searchable in the UI ----------------------
    const searchStatus = await ui.search(browser, "izvoza");
    assert.equal(ui.totalFromStatus(searchStatus, "filtered"), 1, "one hit for the jsonl body");

    // --- re-import dedups: each row keeps its (source, external_id) ------
    const reimport = await ui.importChatExport(browser, jsonlPath, {
      extractMedia: false,
    });
    assert.ok(reimport.ok, `re-import job succeeded: ${reimport.error ?? ""}`);
    assert.ok(
      reimport.value.summary.inserted === 0 && reimport.value.summary.skipped === 3,
      "re-import dedups (0 new, 3 already present) " +
        `(got inserted=${reimport.value.summary.inserted} skipped=${reimport.value.summary.skipped})`,
    );
    assert.equal(
      await browser.execute(() => document.querySelector("#stats")?.textContent),
      "3 messages (telegram: 3)",
      "re-import stored nothing new",
    );
  });
}
