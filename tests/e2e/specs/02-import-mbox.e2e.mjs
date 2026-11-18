// Path: import an .mbox fixture and a bare .eml mail, and confirm they
// landed in the database.

export const name = "import-mbox";

import path from "node:path";

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // --- first import: 8 new messages ------------------------------------
    const summary = await ui.importMbox(browser, ui.archiveMbox);
    assert.deepEqual(
      summary,
      { source: "mbox", seen: 8, inserted: 8, skipped: 0, failed: 0 },
      "import summary (the v2 job result)",
    );
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "8 messages (Mail import: 8)",
      "stats label after import",
    );

    // --- confirmation straight from the database --------------------------
    const db = vaultDbPath(dataDir);
    if (!ctx.sqlite) {
      ctx.log("      (sqlite3 CLI unavailable — skipping direct DB assertions)");
    } else {
      const count = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
      assert.equal(count[0].n, 8, "row count in canonical archive_messages table");

      // Date headers survived parsing as RFC 3339 UTC, including the
      // near-midnight boundary messages the date-range specs rely on.
      const midnight = await ctx.sqlite.query(
        db,
        "SELECT external_id, sent_instant FROM archive_messages WHERE external_id IN ('e2e-m2@archive.local', 'e2e-m3@archive.local') ORDER BY sent_instant",
      );
      assert.equal(midnight.length, 2, "midnight pair imported");
      assert.equal(midnight[0].sent_instant, "2025-10-31T23:59:30Z", "sent_at before midnight");
      assert.equal(midnight[1].sent_instant, "2025-11-01T00:00:30Z", "sent_at after midnight");

      const undated = await ctx.sqlite.query(
        db,
        "SELECT sent_instant FROM archive_messages WHERE external_id = 'e2e-m7@archive.local'",
      );
      assert.equal(undated[0].sent_instant, null, "message without Date header has no resolved instant");

      const chat = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM archive_messages WHERE chat_id = 'archive'",
      );
      assert.equal(chat[0].n, 8, "chat_id is the mbox file stem for every message");

      // FTS5 triggers kept the index in sync; base64 bodies were decoded.
      const fts = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM archive_fts WHERE archive_fts MATCH 'zephyr'",
      );
      assert.equal(fts[0].n, 1, "base64-decoded body is full-text indexed");
      const body = await ctx.sqlite.query(
        db,
        "SELECT body FROM archive_messages WHERE external_id = 'e2e-m6@archive.local'",
      );
      assert.includes(body[0].body, "hidden zephyr signal", "decoded body text stored");
    }

    // --- re-import is idempotent ------------------------------------------
    const reimport = await ui.importMbox(browser, ui.archiveMbox);
    assert.deepEqual(
      reimport,
      { source: "mbox", seen: 8, inserted: 0, skipped: 8, failed: 0 },
      "re-import dedups (0 new, 8 already present)",
    );
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "8 messages (Mail import: 8)",
      "stats unchanged after re-import",
    );
    if (ctx.sqlite) {
      const count = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
      assert.equal(count[0].n, 8, "re-import inserted nothing");
    }

    // --- demo seed adds a second source -----------------------------------
    // (the demo corpus differs between branches, so derive its size)
    const demo = await ui.seedDemo(browser);
    const seeded = /Demo messages: (\d+) new, 0 already present\./.exec(demo);
    assert.ok(seeded, `demo summary shape, got: ${demo}`);
    const demoCount = Number(seeded[1]);
    assert.ok(demoCount >= 4, `expected at least the classic 4 demo messages, got ${demoCount}`);
    assert.equal(
      await (await browser.$("#stats")).getText(),
      `${8 + demoCount} messages (Mail import: 8, demo: ${demoCount})`,
      "stats label counts both sources",
    );

    // --- a bare .eml pick imports through the same mail path ----------
    // (the preview card labels it "Email messages (.eml)" — formatLabel's
    // eml arm — and the confirm step dispatches the same import_mbox the
    // card's button invokes; the mail joins the shared "mbox" namespace
    // with the .eml file stem as its chat.)
    const emlPath = path.join(ui.fixturesDir, "outlook.eml");
    const emlPreview = await ui.invokeCommand(browser, "preview_import", {
      path: emlPath,
    });
    assert.equal(emlPreview.kind, "eml", "preview kind");
    assert.equal(emlPreview.messages_seen, 1, "one mail in the .eml pick");
    const emlSummary = await ui.importMbox(browser, emlPath);
    assert.deepEqual(
      emlSummary,
      { source: "mbox", seen: 1, inserted: 1, skipped: 0, failed: 0 },
      ".eml import summary (one new mail in the mbox namespace)",
    );
    assert.equal(
      await (await browser.$("#stats")).getText(),
      `${9 + demoCount} messages (Mail import: 9, demo: ${demoCount})`,
      "stats label counts the .eml mail under mbox",
    );

    // The dragged-out mail is searchable like any archived message.
    const emlStatus = await ui.search(browser, "quokka");
    assert.equal(ui.totalFromStatus(emlStatus, "filtered"), 1, "one quokka hit");
    if (ctx.sqlite) {
      // File-level confirmation: the .eml mail row lives in the canonical
      // table (the WAL guard keeps CLI reads coherent with the live app
      // — see harness/sqlite.mjs and issue #271).
      const onDisk = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM archive_messages WHERE body LIKE '%quokka%'",
      );
      assert.equal(
        onDisk?.[0]?.n,
        1,
        "the .eml quokka mail row lives in vault.db",
      );
    }
  });
}
