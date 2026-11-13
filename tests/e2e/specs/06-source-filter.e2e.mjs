// Path: filter by source (source chips). Ships with the filtered search UI
// (fm/mailchat-search-ui); skipped on builds whose UI has no chips yet.

export const name = "source-filter";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("source filter UI pending (fm/mailchat-search-ui)");
    }

    await ui.importMbox(browser, ui.archiveMbox);
    await ui.seedDemo(browser);
    // The demo corpus differs between branches; only its identity as a
    // second source with a stable count in the stats matters here.
    const statsText = await (await browser.$("#stats")).getText();
    assert.matches(
      statsText,
      /^(\d+) messages \(mbox: 8, demo: (\d+)\)$/,
      "stats show both sources after import + seed",
    );
    const demoCount = Number(/demo: (\d+)\)$/.exec(statsText)[1]);

    // Chips key on stable instance ids (`data-source` holds the id, the
    // text still reads `label (count)`), so resolve each label to its id
    // through the live `sources_list` instead of assuming label-keyed
    // chips. Chips re-render on every toggle, so always re-query instead
    // of holding references.
    const instances = await ui.invokeCommand(browser, "sources_list");
    const idByLabel = new Map(instances.map((s) => [s.label, s.id]));
    assert.ok(idByLabel.get("mbox"), "mbox instance listed with a stable id");
    assert.ok(idByLabel.get("demo"), "demo instance listed with a stable id");
    const chip = (source) =>
      browser.$(`#source-chips button.chip[data-source="${idByLabel.get(source)}"]`);
    assert.ok(await (await chip("mbox")).isExisting(), "mbox chip rendered");
    assert.ok(await (await chip("demo")).isExisting(), "demo chip rendered");
    assert.includes(await (await chip("mbox")).getText(), "mbox (8)", "chip shows source and count");
    assert.includes(
      await (await chip("demo")).getText(),
      `demo (${demoCount})`,
      "chip shows source and count",
    );

    // Unfiltered: vault lives in both sources — 4 in the fixture's mbox plus
    // however many the demo corpus contributes. Derive that from the UI so
    // the spec does not depend on the demo corpus contents.
    const allStatus = await ui.search(browser, "vault");
    const totalVault = ui.totalFromStatus(allStatus, "filtered");
    assert.ok(totalVault > 4, `expected demo vault matches beyond the fixture, got ${totalVault}`);

    // Select only demo: the demo corpus contributes totalVault - 4 vault
    // matches (the fixture's mbox holds exactly four).
    await (await chip("demo")).click();
    let status = await ui.waitForText(
      browser,
      "#status",
      (text) => !text.startsWith("Searching") && text !== "",
      { timeoutMsg: "demo-filtered search never settled" },
    );
    assert.equal(
      ui.totalFromStatus(status, "filtered"),
      totalVault - 4,
      "demo-only vault matches (all minus the fixture's four)",
    );
    assert.equal(await (await chip("demo")).getAttribute("aria-pressed"), "true", "demo chip pressed");
    const demoRows = (await ui.resultTexts(browser)).join("\n");
    assert.ok(
      !demoRows.includes("obsidian"),
      "demo-filtered rows exclude the fixture's midnight mails",
    );

    // Switch to mbox only.
    await (await chip("demo")).click();
    await (await chip("mbox")).click();
    status = await ui.waitForText(
      browser,
      "#status",
      (text) => !text.startsWith("Searching") && text !== "",
      { timeoutMsg: "mbox-filtered search never settled" },
    );
    assert.equal(ui.totalFromStatus(status, "filtered"), 4, "mbox-only vault matches");

    // Every rendered row belongs to the selected source: the fixture's
    // distinctive "obsidian" keyword exists only in the two midnight mbox
    // mails, so mbox-only rows must contain it and demo-only rows must not.
    const rows = await ui.resultTexts(browser);
    assert.equal(rows.length, 4, "mbox rows rendered");
    const mboxJoined = rows.join("\n");
    assert.ok(
      mboxJoined.includes("obsidian"),
      "mbox-filtered rows include the fixture midnight mails",
    );

    // Deselect -> back to all sources.
    await (await chip("mbox")).click();
    status = await ui.waitForText(
      browser,
      "#status",
      (text) => !text.startsWith("Searching") && text !== "",
      { timeoutMsg: "unfiltered search never settled" },
    );
    assert.equal(ui.totalFromStatus(status, "filtered"), totalVault, "deselecting restores all sources");
  });
}
