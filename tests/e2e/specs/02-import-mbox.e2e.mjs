// Path: import an .mbox fixture and confirm it landed in the database.

export const name = "import-mbox";

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // --- first import: 8 new messages ------------------------------------
    const status = await ui.importMbox(browser, ui.archiveMbox);
    assert.includes(
      status,
      "Imported 8 new, 0 already present (of 8 in mbox).",
      "import summary",
    );
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "8 messages (mbox: 8)",
      "stats label after import",
    );

    // --- confirmation straight from the database --------------------------
    const db = vaultDbPath(dataDir);
    if (!ctx.sqlite) {
      ctx.log("      (sqlite3 CLI unavailable — skipping direct DB assertions)");
    } else {
      const count = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM messages");
      assert.equal(count[0].n, 8, "row count in messages table");

      // Date headers survived parsing as RFC 3339 UTC, including the
      // near-midnight boundary messages the date-range specs rely on.
      const midnight = await ctx.sqlite.query(
        db,
        "SELECT external_id, sent_at FROM messages WHERE external_id IN ('e2e-m2@archive.local', 'e2e-m3@archive.local') ORDER BY sent_at",
      );
      assert.equal(midnight.length, 2, "midnight pair imported");
      assert.equal(midnight[0].sent_at, "2025-10-31T23:59:30Z", "sent_at before midnight");
      assert.equal(midnight[1].sent_at, "2025-11-01T00:00:30Z", "sent_at after midnight");

      const undated = await ctx.sqlite.query(
        db,
        "SELECT sent_at FROM messages WHERE external_id = 'e2e-m7@archive.local'",
      );
      assert.equal(undated[0].sent_at, "", "message without Date header has empty sent_at");

      const chat = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM messages WHERE chat_id = 'archive'",
      );
      assert.equal(chat[0].n, 8, "chat_id is the mbox file stem for every message");

      // FTS5 triggers kept the index in sync; base64 bodies were decoded.
      const fts = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM messages_fts WHERE messages_fts MATCH 'zephyr'",
      );
      assert.equal(fts[0].n, 1, "base64-decoded body is full-text indexed");
      const body = await ctx.sqlite.query(
        db,
        "SELECT body FROM messages WHERE external_id = 'e2e-m6@archive.local'",
      );
      assert.includes(body[0].body, "hidden zephyr signal", "decoded body text stored");
    }

    // --- re-import is idempotent ------------------------------------------
    const reimport = await ui.importMbox(browser, ui.archiveMbox);
    assert.includes(reimport, "Imported 0 new, 8 already present", "re-import dedups");
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "8 messages (mbox: 8)",
      "stats unchanged after re-import",
    );
    if (ctx.sqlite) {
      const count = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM messages");
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
      `${8 + demoCount} messages (mbox: 8, demo: ${demoCount})`,
      "stats label counts both sources",
    );
  });
}
