// Path: import a Signal conversation .json export end-to-end through the
// running app, and use the archive through the real search UI afterwards.
//
// The native GTK file chooser behind "Import chat export…" cannot be
// automated over WebDriver (and `window.__TAURI_INTERNALS__` is
// non-configurable, so the dialog IPC cannot be intercepted), so this
// spec invokes the very same `import_chat_export` backend command the
// button handler invokes, with a real file, then reloads the app so the
// normal boot path renders the stats label and source chips from the
// freshly imported archive. Everything downstream — stats, chips, search,
// highlights, detail view, dedup, and the loud failure on an unrecognized
// file — runs through the real product.
export const name = "signal-import";

import { copyFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fixturesDir } from "../harness/ui.mjs";

const signalFixture = path.join(fixturesDir, "signal-conversation.json");

/**
 * Invoke a Tauri command in the app and await its result, reporting
 * `{ ok, value, error }`. The async arrow makes WebKitWebDriver run this
 * through its async-script endpoint, whose error wrapper preserves the
 * backend error text — a rejected invoke over the sync-script endpoint
 * surfaces as an opaque "Could not parse script result" instead.
 */
async function invoke(browser, command, args = {}) {
  try {
    const value = await browser.execute(
      async (cmd, payload) => window.__TAURI_INTERNALS__.invoke(cmd, payload),
      command,
      args,
    );
    return { ok: true, value };
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // --- import via the real backend command ------------------------------
    const summary = await invoke(browser, "import_chat_export", { path: signalFixture });
    assert.ok(summary.ok, `import command succeeded: ${summary.error ?? ""}`);
    const s = summary.value.summary;
    assert.ok(s.source === "signal" && s.seen === 7 && s.inserted === 5 && s.skipped === 0 && s.failed === 2,
      "import summary: 5 archived, 2 broken entries counted, service entries skipped " +
      `(got source=${s.source} seen=${s.seen} inserted=${s.inserted} skipped=${s.skipped} failed=${s.failed})`,
    );

    // --- reload: normal boot renders stats + chips from the archive -------
    await browser.execute(() => location.reload());
    const statsText = await ui.waitForApp(browser);
    assert.equal(statsText, "5 messages (signal: 5)", "stats label after signal import");

    const chip = await browser.$('#source-chips button.chip[data-source="signal"]');
    assert.ok(await chip.isExisting(), "signal source chip rendered");
    assert.includes(await chip.getText(), "signal (5)", "chip shows source and count");

    // --- the imported texts are searchable in the UI ----------------------
    const searchStatus = await ui.search(browser, "reagiram");
    assert.equal(ui.totalFromStatus(searchStatus, "filtered"), 1, "one reagiram hit");

    const hits = await browser.$$("#results li.hit-item");
    assert.equal(hits.length, 1, "hit item rendered");
    const hitText = await hits[0].getText();
    assert.ok(hitText.includes("reagiram"), "hit body shows the message text");
    assert.ok(hitText.includes("+385912345678"), "hit meta shows the author");
    // M3.2: results are a flat list — the conversation label rides in
    // the row's meta line instead of a group header.
    const hitMeta = await hits[0].$(".meta").getText();
    assert.includes(hitMeta, "signal", "flat row meta shows the signal source");

    // Highlighted query terms in the rendered row.
    const marks = await browser.$$("#results li.hit-item mark");
    assert.ok((await marks.length) > 0, "query terms highlighted with <mark>");

    // --- detail view: archived metadata + thread context ------------------
    await hits[0].click();
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "clicking the signal hit did not open the detail panel" },
    );
    const table = await (await browser.$("#detail .detail-card .meta-table")).getText();
    assert.ok(/source\s*\n?\s*signal/.test(table), "detail shows source signal");
    assert.includes(
      table,
      "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01/9a8b7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
      "detail shows the conversation-namespaced external id",
    );
    assert.includes(table, "+385912345678", "detail shows the author");
    const threadRows = await browser.$$("#detail ol.thread li.thread-row");
    assert.equal(threadRows.length, 5, "all 5 conversation messages as thread neighbours");

    // M3.2 verified-conversation context: neighbours are clickable and
    // the anchor marker follows (the unverified-grouping counterpart is
    // the detail-view spec).
    const links = await browser.$$("#detail li.thread-row.link");
    assert.ok(links.length > 0, "verified conversation rows are clickable links");
    const linkMeta = await links[0].$(".meta").getText();
    const linkAuthor = linkMeta.split("·")[0].trim();
    await links[0].click();
    await browser.waitUntil(
      async () => (await browser.$("#detail .detail-card h2").getText()) === linkAuthor,
      { timeoutMsg: "clicking a neighbour did not switch the detail view" },
    );
    assert.equal(
      await browser.$$("#detail li.thread-row.current").length,
      1,
      "current marker moved with the view",
    );
    await browser.execute(() => document.activeElement?.blur());
    await browser.keys("Escape");
    // --- re-import dedups --------------------------------------------------
    const reimport = await invoke(browser, "import_chat_export", { path: signalFixture });
    assert.ok(reimport.ok, `re-import command succeeded: ${reimport.error ?? ""}`);
    assert.ok(
      reimport.value.summary.inserted === 0 &&
        reimport.value.summary.skipped === 5 &&
        reimport.value.summary.failed === 2,
      "re-import dedups (0 new, 5 already present) " +
      `(got inserted=${reimport.value.summary.inserted} skipped=${reimport.value.summary.skipped} failed=${reimport.value.summary.failed})`,
    );

    // --- renamed re-export dedups by conversation uuid ---------------------
    const renamed = path.join(dataDir, "Renamed chat.json");
    await copyFile(signalFixture, renamed);
    const renamedSummary = await invoke(browser, "import_chat_export", { path: renamed });
    assert.ok(renamedSummary.ok, `renamed import command succeeded: ${renamedSummary.error ?? ""}`);
    assert.ok(
      renamedSummary.value.summary.inserted === 0 && renamedSummary.value.summary.skipped === 5,
      "renamed re-export dedups by conversation uuid " +
      `(got inserted=${renamedSummary.value.summary.inserted} skipped=${renamedSummary.value.summary.skipped})`,
    );

    // --- unrecognized file fails loudly, stores nothing --------------------
    const garbage = path.join(dataDir, "not-a-chat-export.json");
    await writeFile(garbage, '{"foo": 1}');
    const garbageResult = await invoke(browser, "import_chat_export", { path: garbage });
    assert.ok(!garbageResult.ok, "unrecognized file must fail the import command");
    assert.includes(garbageResult.error, "unrecognized chat export format");
    assert.includes(garbageResult.error, "Signal conversation .json",
      "error mentions the Signal format explicitly");
    assert.equal(
      await browser.execute(() => document.querySelector("#stats")?.textContent),
      "5 messages (signal: 5)",
      "failed import stored nothing",
    );

    // Close the detail panel so the final screenshot shows the import
    // error surface next to the untouched archive.
    await browser.execute(() => {
      document.querySelector("#detail")?.classList.remove("open");
      document.querySelector("#search").value = "";
      document.querySelector("#results").replaceChildren();
    });
  });
}
