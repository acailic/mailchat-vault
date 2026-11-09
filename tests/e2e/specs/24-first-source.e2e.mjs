// Path: U02/U04 — the first-source journey on the real app.
//
// 1. A fresh boot on an empty archive presents the first-run card
//    ("Add your first source" + "Try a sample archive"), not a dead
//    search prompt.
// 2. After an import, "All messages" browses newest-first with zero
//    interaction (U04: the just-imported archive is visible; the old
//    behavior showed 0 rows until a filter was set — F06).
// 3. "Try a sample archive" opens the isolated demo: its stats/browse
//    read the in-memory demo vault; the personal vault.db is untouched
//    (message count, source instances — verified against SQLite); every
//    personal-vault write refuses while it is open; "Leave sample"
//    restores the personal archive exactly.

export const name = "first-source-journey";

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    const db = vaultDbPath(dataDir);

    // --- 1. first run: the CTA owns the empty archive ---------------------
    await ui.waitForApp(browser);
    assert.equal(await (await browser.$("#stats")).getText(), "empty archive");
    assert.ok(
      await (await browser.$("#first-run")).isDisplayed(),
      "first-run card visible on an empty archive",
    );
    assert.equal(
      await (await browser.$("#demo-banner")).isDisplayed(),
      false,
      "no demo banner on the personal vault",
    );

    // The primary CTA opens the Add source view at the import controls.
    await (await browser.$("#first-run-import-btn")).click();
    await browser.waitUntil(
      async () =>
        (await (await browser.$("#app")).getAttribute("data-view")) === "sources",
      { timeoutMsg: "the first-run CTA did not open the Add source view" },
    );
    assert.ok(
      await (await browser.$("#import-pick-btn")).isDisplayed(),
      "the import picker button is the offered action",
    );

    // --- 2. the isolated sample archive (U02), entered from the card -----
    await ui.openView(browser, "workspace"); // the card lives in the results area
    await (await browser.$("#first-run-demo-btn")).click();
    // Entering replaces the empty personal view with the demo corpus.
    await ui.waitForText(browser, "#stats", "6 messages (demo: 6)");
    assert.ok(
      await (await browser.$("#demo-banner")).isDisplayed(),
      "the demo banner explains the isolation",
    );
    await ui.waitForText(browser, "#status", "1–6 of 6");
    assert.equal((await ui.resultTexts(browser)).length, 6, "the demo archive browses");
    // Demo reads answer from the demo vault: a term only the demo corpus
    // carries ("sqlite" appears nowhere in the fixtures).
    const demoHit = await ui.search(browser, "sqlite");
    assert.matches(demoHit, /^1–1 of 1/, "demo content is searchable inside the sample");

    // The personal vault underneath was never touched (it is still the
    // empty vault from first boot — the import comes after leaving).
    if (ctx.sqlite) {
      const messages = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
      assert.equal(messages[0].n, 0, "the personal vault is still empty inside the demo");
      const demoSources = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM source_instances WHERE label = 'demo'",
      );
      assert.equal(demoSources[0].n, 0, "no demo source leaked into the personal vault");
    }

    // Every personal-vault write refuses while the demo is open — the
    // structural boundary behind "demo never mixes into the vault".
    const refusal = await ui
      .invokeCommand(browser, "import_mbox", { path: ui.archiveMbox })
      .then(() => null, (e) => String(e));
    assert.ok(
      refusal !== null && refusal.includes("sample archive"),
      `import under the demo refuses with guidance (got: ${refusal})`,
    );
    const jobRefusal = await ui
      .invokeCommand(browser, "job_submit", {
        kind: "import_mbox",
        params: { path: ui.archiveMbox },
      })
      .then(() => null, (e) => String(e));
    assert.ok(
      jobRefusal !== null && jobRefusal.includes("sample archive"),
      `job submit under the demo refuses (got: ${jobRefusal})`,
    );
    // The UI locks the source controls while the demo is open.
    assert.equal(
      await (await browser.$("#import-pick-btn")).isEnabled(),
      false,
      "import controls are locked under the demo",
    );

    // Leaving restores the personal archive exactly — which is still
    // empty, so the first-run card comes back.
    await (await browser.$("#demo-leave-btn")).click();
    await ui.waitForText(browser, "#stats", "empty archive");
    assert.ok(
      await (await browser.$("#first-run")).isDisplayed(),
      "leaving an empty personal vault returns to the first-run card",
    );
    assert.equal(
      await (await browser.$("#demo-banner")).isDisplayed(),
      false,
      "the banner is gone after leaving",
    );
    assert.equal(
      await (await browser.$("#import-pick-btn")).isEnabled(),
      true,
      "import controls unlock after leaving",
    );

    // --- 3. import → "All messages" browses without any typing (U04) ------
    const summary = await ui.importMbox(browser, ui.archiveMbox);
    assert.equal(summary.inserted, 8, "the fixture imports 8 messages");
    if (ctx.sqlite) {
      const messages = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
      assert.equal(messages[0].n, 8, "the import landed in the personal vault after leaving the demo");
    }
    await ui.openView(browser, "workspace");
    await ui.waitForText(browser, "#status", "1–8 of 8");
    const rows = await ui.resultTexts(browser);
    assert.equal(rows.length, 8, "the browse shows every imported message");
    assert.ok(
      (await (await browser.$("#first-run")).isDisplayed()) === false,
      "the first-run card is gone once the archive has messages",
    );
    // The demo's content is not in the personal vault: the demo-only
    // term finds nothing while a fixture term does.
    const personal = await ui.search(browser, "sqlite");
    assert.matches(personal, /^0 results/, "demo-only content is not in the personal vault");
    const fixture = await ui.search(browser, "vault");
    assert.matches(fixture, /1–4 of 4 for “vault”/, "the fixture's four vault mails match");
  });
}
