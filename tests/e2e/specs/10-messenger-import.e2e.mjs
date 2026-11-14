// Path: import a Messenger (Facebook) "Download Your Information" JSON
// export end-to-end through the running app: the DYI tree pick — the
// same folder the "Import folder" button hands the backend — previews
// as kind "messenger" (rendered as "Messenger (Facebook) export" by
// formatLabel, pinned by importview.test.ts), imports through the very
// same `import_chat_export` job the preview card's confirm button
// submits (`job_submit`), and the archived thread is searchable through
// the real search UI afterwards. The two thread parts merge in numeric
// order, the photo message's real bytes land in the media store, and
// the reactions-only entry is skipped without counting as a failure.
//
// The native GTK folder chooser behind "Import folder…" cannot be
// automated over WebDriver (the documented limitation every import
// spec works around), so the spec drives the seams the button handler
// drives — `preview_import`, then the `job_submit` confirm path on the
// same path — and reloads so the normal boot path renders the stats
// label from the freshly imported archive.
export const name = "messenger-import";

import path from "node:path";
import { fixturesDir } from "../harness/ui.mjs";

// A trimmed DYI tree: one thread (ana_1111) in two part files plus the
// photo its third message references.
const messengerDir = path.join(fixturesDir, "messenger-dyi");

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // --- the folder pick previews as a Messenger DYI export ----------
    // (kind "messenger" is what the preview card labels "Messenger
    // (Facebook) export" — the mapping is unit-tested, the e2e seam is
    // the preview command the picker result feeds.)
    const preview = await ui.invokeCommand(browser, "preview_import", {
      path: messengerDir,
    });
    assert.equal(preview.kind, "messenger", "preview kind");
    assert.equal(preview.source, "messenger", "preview source namespace");
    assert.equal(
      preview.messages_seen,
      5,
      "3 stored from message_1 (the reactions-only entry is skipped) + 2 from message_2",
    );
    assert.equal(preview.scan_complete, true, "small exports scan completely");
    assert.equal(preview.parse_failures, 0, "every entry parsed");
    assert.equal(preview.media_referenced, 1, "the photo message references ana.jpg");
    assert.equal(preview.media_missing, 0, "the photo is part of the picked tree");
    assert.equal(preview.samples.length, 3, "samples are capped at three");
    assert.equal(preview.samples[0].author, "Ana Peric", "samples carry authors");

    // --- confirm: the same job the preview card's button submits -----
    const outcome = await ui.importChatExport(browser, messengerDir);
    assert.ok(outcome.ok, `import job succeeded: ${outcome.error ?? ""}`);
    assert.deepEqual(
      outcome.value.summary,
      { source: "messenger", seen: 5, inserted: 5, skipped: 0, failed: 0 },
      "import summary (both parts, thread merged)",
    );
    // The pick is the DYI root, so the media pass runs: the referenced
    // photo's bytes are stored and linked onto its message.
    const media = outcome.value.media;
    assert.ok(media, "media pass ran for the tree pick");
    assert.equal(media.referenced, 1, "one referenced media file");
    assert.equal(media.stored, 1, "ana.jpg bytes stored");
    assert.equal(media.missing, 0, "nothing missing from the export");
    assert.equal(media.linked, 1, "the photo message carries its media link");

    // --- reload: normal boot renders stats from the archive -----------
    await browser.execute(() => location.reload());
    const statsText = await ui.waitForApp(browser);
    assert.equal(statsText, "5 messages (messenger: 5)", "stats label after messenger import");

    // --- the imported texts are searchable in the UI ------------------
    const searchStatus = await ui.search(browser, "zdravo");
    assert.equal(ui.totalFromStatus(searchStatus, "filtered"), 1, "one zdravo hit");

    const hits = await browser.$$("#results li.hit-item");
    assert.equal(hits.length, 1, "hit item rendered");
    const hitText = await hits[0].getText();
    assert.ok(hitText.includes("Zdravo"), "hit body shows the message text");
    assert.ok(hitText.includes("Ana Peric"), "hit meta shows the author");
    // Flat row: the conversation label rides in the row's meta line.
    const hitMeta = await hits[0].$(".meta").getText();
    assert.includes(hitMeta, "messenger", "flat row meta shows the messenger source");

    // --- re-import dedups on the generated ids ------------------------
    const reimport = await ui.importChatExport(browser, messengerDir);
    assert.ok(reimport.ok, `re-import job succeeded: ${reimport.error ?? ""}`);
    assert.ok(
      reimport.value.summary.inserted === 0 && reimport.value.summary.skipped === 5,
      "re-import dedups (0 new, 5 already present) " +
      `(got inserted=${reimport.value.summary.inserted} skipped=${reimport.value.summary.skipped})`,
    );
    assert.equal(
      await browser.execute(() => document.querySelector("#stats")?.textContent),
      "5 messages (messenger: 5)",
      "stats unchanged after re-import",
    );
  });
}
