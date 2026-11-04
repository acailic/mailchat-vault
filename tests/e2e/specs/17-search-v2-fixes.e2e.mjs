// Path: M3.2 fix-round regressions, driven through the real v2 IPC and
// the running app:
//   1. Multi-row "Load earlier" context pagination: a 16-message
//      verified Signal conversation - the initial pane holds 8 earlier
//      rows + the anchor; loading the 7-row second page must splice it
//      at the front oldest -> newest (the pre-fix unshift loop rendered
//      the page reversed at the top).
//   2. Two source instances sharing one display label: the shared chip
//      expands to one applied chip per matching instance id, and
//      removing one applied chip clears every id the label expanded to
//      (the pre-fix sibling removed a single id and the filter silently
//      survived).
//   3. A selection built across result pages: the button counts checks
//      from both pages, the backend writes every checked id, and
//      reading + Back restores the same page, count, selection, and
//      scroll (A07 same-snapshot invariant).
//
// The native GTK save dialog cannot be automated over WebDriver (same
// limitation as spec 11), so the export click path's id assembly is
// pinned by src/main.test.ts ("exporting a selection built across pages
// exports every checked id"); this spec drives everything around it.

export const name = "search-v2-fixes";

import { writeFile, readFile, mkdtemp, rm } from "node:fs/promises";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

/** Invoke a Tauri command and await { ok, value, error }. */
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

/** Read live DOM data in one execute round-trip. */
const readDom = (browser, fn) => browser.execute(fn);

/** Wait until a search settles (a result/summary line, not the prompt). */
const settle = (browser, ui) =>
  ui.waitForText(
    browser,
    "#status",
    (text) => !text.startsWith("Searching") && text !== "" && !text.startsWith("Type"),
    { timeoutMsg: "search never settled" },
  );

/** Scroll the detail/results surface into view, then screenshot. */
async function scrollAndShoot(browser, filename) {
  await browser.execute(() => {
    for (const sel of ["#detail", "#active-filter-chips", "#export-controls"]) {
      const el = document.querySelector(sel);
      if (el) el.scrollIntoView({ block: "center" });
    }
  });
  await saveScreenshot(browser, filename);
}

/** Viewport screenshot into the evidence dir (when configured). */
async function saveScreenshot(browser, filename) {
  const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR;
  if (!evidenceDir) {
    return;
  }
  try {
    const base64 = await browser.takeScreenshot();
    writeFileSync(path.join(evidenceDir, filename), Buffer.from(base64, "base64"));
    process.stdout.write(`screenshot: ${path.join(evidenceDir, filename)}\n`);
  } catch (e) {
    process.stdout.write(`screenshot unavailable (${String(e?.message ?? e)})\n`);
  }
}

/** The thread-row data-ids, in rendered order. */
const threadIds = (browser) =>
  readDom(browser, () =>
    [...document.querySelectorAll("#detail ol.thread li.thread-row")].map((r) => r.dataset.id),
  );

/** One bulk mbox message body; 60 of these make a 2-page result set. */
const bulkMessage = (i) =>
  [
    "From sender@bulk.local Thu Jan  1 00:00:00 2026",
    "From: sender@bulk.local",
    "To: reader@bulk.local",
    `Subject: bulk ${String(i + 1).padStart(2, "0")}`,
    "Date: Thu, 01 Jan 2026 00:00:00 +0000",
    "",
    `bulk vault ${String(i + 1).padStart(2, "0")}`,
    "",
  ].join("\n");

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath, log } = ctx;

  // ---------------------------------------------------------------------
  // 1. Multi-row "Load earlier" page splices in chronological order
  // ---------------------------------------------------------------------
  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("v2 search UI pending (M3.2)");
    }

    // One Signal conversation, 16 chronologically ascending messages:
    // opening the newest leaves 15 earlier rows, so the first context
    // page holds 8 and "Load earlier" fetches a 7-row second page.
    const uuidBase = "1f2e3d4c-5b6a-4901-8c2d-";
    const messages = Array.from({ length: 16 }, (_, i) => ({
      id: `${uuidBase}${String(i + 1).padStart(2, "0")}`,
      type: i % 2 === 0 ? "incoming" : "outgoing",
      conversationId: "aabbccdd-1122-4333-8444-556677889900",
      source: i % 2 === 0 ? "+385991000000.1" : undefined,
      sent_at: 1700000000000 + i * 60000,
      timestamp: 1700000000000 + i * 60000,
      body: `ctxmsg ${String(i + 1).padStart(2, "0")}`,
      schemaVersion: 35,
    }));
    const dir = await mkdtemp(path.join(tmpdir(), "mc17-"));
    const fixture = path.join(dir, "conversation.json");
    await writeFile(fixture, JSON.stringify(messages));

    const imported = await invoke(browser, "import_chat_export", {
      path: fixture,
      extract_media: false,
    });
    assert.ok(imported.ok, `signal import succeeded: ${imported.error ?? ""}`);
    assert.equal(
      imported.value.summary.inserted,
      16,
      `16 messages archived (got ${JSON.stringify(imported.value?.summary)})`,
    );

    await browser.execute(() => location.reload());
    await ui.waitForApp(browser);

    const status = await ui.search(browser, "ctxmsg 16");
    assert.equal(
      ui.totalFromStatus(status, "filtered"),
      1,
      `the newest message is searchable (status: ${status})`,
    );
    const hits = await browser.$$("#results li.hit-item");
    assert.equal(hits.length, 1, "exactly one hit for the anchor query");

    await hits[0].click();
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "opening the hit did not open the reading pane" },
    );

    // Initial context page: 8 earlier rows + the anchor, oldest -> newest.
    const idsBefore = await threadIds(browser);
    assert.equal(idsBefore.length, 9, "8 context rows + the anchor render");
    assert.ok(
      idsBefore.map(Number).every((v, i, a) => i === 0 || v > a[i - 1]),
      `initial context rows are chronological (got ${idsBefore.join(",")})`,
    );

    const earlier = await browser.$("#detail .load-earlier-btn");
    assert.ok(await earlier.isEnabled(), "more earlier context is advertised");

    // The fix: the multi-row page splices at the front oldest -> newest.
    await earlier.click();
    await browser.waitUntil(
      async () => (await threadIds(browser)).length === 16,
      { timeoutMsg: "loading the earlier page did not add the 7 remaining rows" },
    );
    const idsAfter = await threadIds(browser);
    assert.equal(idsAfter.length, 16, "the whole conversation is held after one Load earlier");
    assert.ok(
      idsAfter.map(Number).every((v, i, a) => i === 0 || v > a[i - 1]),
      `merged thread stays oldest -> newest after a multi-row prepend (got ${idsAfter.join(",")})`,
    );
    assert.equal(
      (await browser.$$("#detail ol.thread li.thread-row.current")).length,
      1,
      "exactly one current row",
    );
    assert.equal(
      await browser.$("#detail .load-earlier-btn").getAttribute("disabled"),
      "true",
      "Load earlier is spent after the last page",
    );

    await scrollAndShoot(browser, "17-load-earlier-merged.png");
    await rm(dir, { recursive: true, force: true });
  });

  // ---------------------------------------------------------------------
  // 2. Colliding display labels: union at filter time, clear-all at
  //    removal time
  // ---------------------------------------------------------------------
  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    if (!sqlite) {
      throw new Error("sqlite3 CLI unavailable - this spec needs direct DB seeding");
    }
    const db = vaultDbPath(dataDir);

    // Two IMAP instances with one display label (the real-world shape:
    // same host:port:user reachable over two TLS modes - display_label()
    // omits the TLS mode, connector_key() carries it), each with its own
    // archived messages so the chips render and the counts prove which
    // instances the filter matched.
    const label = "imap:127.0.0.1:993:alice";
    const mkConfig = (tls) =>
      JSON.stringify({
        host: "127.0.0.1",
        port: 993,
        tls_mode: tls,
        username: "alice",
        folders: ["INBOX"],
      });
    const seedInstance = (id, tls) => {
      const key = `imap|127.0.0.1|993|${tls}|alice`;
      const config = mkConfig(tls).replace(/'/g, "''");
      return `INSERT INTO source_instances (id, kind, label, connector_key, config_json, state, excluded)
              VALUES ('${id}', 'imap', '${label}', '${key}', '${config}', 'connected', 0)`;
    };
    const seedConversation = (convId, instId) =>
      `INSERT INTO conversations (id, source_instance_id, legacy_chat_key, label, kind, confidence)
       VALUES ('${convId}', '${instId}', 'inbox', 'Inbox', 'mail', 'legacy_unresolved')`;
    const seedMessage = (instId, convId, extId, body, minute) =>
      `INSERT INTO archive_messages
         (source_instance_id, conversation_id, external_id, channel, chat_id, author, subject,
          body, sent_instant, original_timestamp, timezone, time_confidence, content_hash)
       VALUES
         ('${instId}', '${convId}', '${extId}', '', 'inbox', 'Alice', '',
          '${body}', '2026-01-01T10:${minute}:00Z', '2026-01-01T10:${minute}:00Z', '', 'legacy_assumed', '')`;

    await sqlite.query(db, seedInstance("inst-a", "implicit-tls"));
    await sqlite.query(db, seedInstance("inst-b", "starttls"));
    await sqlite.query(db, seedConversation("conv-a", "inst-a"));
    await sqlite.query(db, seedConversation("conv-b", "inst-b"));
    await sqlite.query(db, seedMessage("inst-a", "conv-a", "a1", "alpha msg", "00"));
    await sqlite.query(db, seedMessage("inst-a", "conv-a", "a2", "alpha msg", "01"));
    await sqlite.query(db, seedMessage("inst-b", "conv-b", "b1", "beta msg", "02"));
    await sqlite.query(db, seedMessage("inst-b", "conv-b", "b1b", "beta msg", "03"));

    // Boot the chips from the freshly seeded rows.
    await browser.execute(() => location.reload());
    await ui.waitForApp(browser);

    // One sidebar chip per message-bearing instance; both share the label.
    const sharedChips = await browser.$$(".chip[data-source='imap:127.0.0.1:993:alice']");
    assert.equal(sharedChips.length, 2, "two sidebar chips render for the shared label");

    // Clicking the shared chip expands the label to BOTH instance ids:
    // the filtered browse matches both instances' messages and one
    // applied chip per id renders.
    await sharedChips[0].click();
    await browser.waitUntil(
      async () => (await browser.$$("#active-filter-chips .filter-chip")).length === 2,
      { timeoutMsg: "the shared label did not expand to two applied chips" },
    );
    const appliedChips = await browser.$$("#active-filter-chips .filter-chip");
    for (const chip of appliedChips) {
      assert.includes(await chip.getText(), label, "the applied chip carries the shared label");
    }
    const statusUnion = await settle(browser, ui);
    await scrollAndShoot(browser, "17-colliding-label-chips.png");
    assert.equal(
      ui.totalFromStatus(statusUnion, "filtered"),
      4,
      "the chip matches every instance carrying the label (4 messages across both)",
    );

    // Removing one applied chip must clear every id the label expanded
    // to - the pre-fix sibling removed a single id, writeFilters mapped
    // the surviving id back to the same label, runSearch re-expanded it,
    // and both chips silently re-rendered with the filter still applied.
    await appliedChips[0].click();
    await browser.waitUntil(
      async () => (await browser.$$("#active-filter-chips .filter-chip")).length === 0,
      { timeoutMsg: "removing the chip left applied filter chips behind" },
    );
    await ui.waitForText(browser, "#status", (text) => text.startsWith("Type something"), {
      timeoutMsg: "the filter did not reset to the empty-prompt state",
    });
    log("    colliding-label chip: matched 4 messages across both ids, removal cleared both");
  });

  // ---------------------------------------------------------------------
  // 3. Cross-page selection: announce, export, and Back invariants
  // ---------------------------------------------------------------------
  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("v2 search UI pending (M3.2)");
    }

    // 60 bulk messages: one page holds 50, so a second page exists.
    const dir = await mkdtemp(path.join(tmpdir(), "mc17b-"));
    const mboxPath = path.join(dir, "bulk.mbox");
    await writeFile(mboxPath, Array.from({ length: 60 }, (_, i) => bulkMessage(i)).join("\n"));

    await ui.importMbox(browser, mboxPath);
    const status = await ui.search(browser, "bulk");
    assert.equal(
      ui.totalFromStatus(status, "filtered"),
      60,
      `all 60 bulk messages match (status: ${status})`,
    );

    const rows1 = await browser.$$("#results li.hit-item");
    assert.equal(rows1.length, 50, "the first page holds the 50-row page size");
    const firstPageId = await rows1[0].getAttribute("data-id");

    // Check a row on page 1, then move to page 2 and check another.
    await (await rows1[0].$(".row-select")).click();
    await (await browser.$("#pagination .page-btn:last-child")).click();
    await browser.waitUntil(
      async () => (await browser.$$("#results li.hit-item")).length === 10,
      { timeoutMsg: "Next did not show the second page" },
    );
    const rows2 = await browser.$$("#results li.hit-item");
    const secondPageId = await rows2[0].getAttribute("data-id");
    await (await rows2[0].$(".row-select")).click();

    // The announced set spans both pages...
    const exportBtn = await browser.$("#export-selection-btn");
    assert.includes(
      await exportBtn.getText(),
      "Export selected (2)",
      "the button counts checks from both pages",
    );
    await scrollAndShoot(browser, "17-cross-page-selection.png");

    // ...and the backend writes every checked id (the save-dialog click
    // path's id assembly is pinned by main.test.ts; the native GTK
    // dialog cannot be driven here).
    const exportFile = path.join(dir, "selection.txt");
    const exported = await invoke(browser, "export_messages_v2", {
      ids: [Number(firstPageId), Number(secondPageId)],
      format: "text",
      path: exportFile,
    });
    assert.ok(exported.ok, `export succeeded: ${exported.error ?? ""}`);
    assert.equal(exported.value.written, 2, "both checked messages exported");
    assert.equal(exported.value.missing, 0, "no checked message vanished");
    const text = await readFile(exportFile, "utf8");
    assert.includes(text, "bulk vault 01", "the page-1 check is in the export");
    assert.includes(text, "bulk vault 51", "the page-2 check is in the export");

    // A07 reading + Back: same-snapshot count, selection, and scroll.
    await readDom(browser, () => {
      const list = document.querySelector("#results");
      list.scrollTop = 1200;
    });
    const scrollBefore = await readDom(browser, () => document.querySelector("#results").scrollTop);
    await rows2[2].click();
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "opening a hit did not open the reading pane" },
    );
    await (await browser.$("#detail .close-btn")).click();
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") === null),
      { timeoutMsg: "Back did not close the reading pane" },
    );
    assert.equal(
      (await browser.$$("#results li.hit-item")).length,
      10,
      "Back restores the same page (no fabricated rows)",
    );
    assert.equal(
      ui.totalFromStatus(await browser.$("#status").getText(), "filtered"),
      60,
      "Back restores the same-snapshot count",
    );
    const checkedAfter = await readDom(browser, () =>
      document.querySelectorAll("#results input.row-select:checked").length,
    );
    assert.equal(checkedAfter, 1, "the page-2 check survives reading + Back");
    const scrollAfter = await readDom(browser, () => document.querySelector("#results").scrollTop);
    assert.ok(
      Math.abs(scrollAfter - scrollBefore) <= 2,
      `scroll restored across reading + Back (${scrollBefore} -> ${scrollAfter})`,
    );

    await rm(dir, { recursive: true, force: true });
  });
}
