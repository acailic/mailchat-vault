// Path: v2 detail view (M3.2) — the reading pane over the canonical
// store. The fixture's mbox chat is grouped only by its file name (no
// reply-graph evidence), which M1/M3 treat honestly as an *unverified*
// grouping: the pane shows the selected message and a "could not be
// verified" notice, never guessed neighbours. Verified-conversation
// context (neighbours, load earlier/later) is covered by the
// signal-import spec (09), whose conversations carry stable keys.

export const name = "detail-view";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("detail view UI pending (fm/mailchat-search-ui)");
    }

    await ui.importMbox(browser, ui.archiveMbox);
    const status = await ui.search(browser, "vault");
    assert.equal(ui.totalFromStatus(status, "filtered"), 4, "four vault hits");

    // State checks for the panel go through page JS: WebKit's WebDriver
    // serves stale findElement matches for `#detail.open` after the class
    // changes, while execute() always reads the live DOM.
    const detailOpen = () =>
      browser.execute(() => document.querySelector("#detail.open") !== null);

    // Clicking a hit opens the detail panel with full metadata.
    const hits = await browser.$$("#results li.hit-item");
    assert.equal(hits.length, 4, "hit items rendered");
    const firstMeta = await hits[0].$(".meta").getText();
    await hits[0].click();
    await browser.waitUntil(async () => await detailOpen(), {
      timeoutMsg: "clicking a hit did not open the detail panel",
    });

    const card = await browser.$("#detail .detail-card");
    const title = await card.$("h2").getText();
    assert.ok(
      firstMeta.startsWith(title),
      `detail title is the hit author: ${title} vs ${firstMeta}`,
    );

    // The meta table exposes the archival facts.
    const table = await card.$(".meta-table").getText();
    assert.ok(/source\s*\n?\s*mbox/.test(table.replace(/  +/g, " ")), "meta table shows source");
    assert.includes(table, "e2e-m", "meta table shows the external message id");

    // The reading actions are present: copy, copy link, export.
    for (const selector of [".copy-btn", ".copy-link-btn", ".export-selection-btn"]) {
      assert.ok(
        await (await browser.$(`#detail ${selector}`)).isExisting(),
        `detail header renders ${selector}`,
      );
    }

    // Unverified grouping: the anchor alone plus the honest notice.
    const threadRows = await browser.$$("#detail ol.thread li.thread-row");
    assert.equal(threadRows.length, 1, "unverified grouping shows the anchor only");
    assert.equal(
      await browser.$$("#detail li.thread-row.current").length,
      1,
      "the anchor is the current row",
    );
    assert.equal(
      await browser.$$("#detail li.thread-row.link").length,
      0,
      "no clickable neighbours are guessed for an unverified grouping",
    );
    const pane = await browser.$("#detail").getText();
    assert.includes(
      pane,
      "Conversation could not be verified",
      "the pane explains the unverified grouping",
    );

    // Arrow keys have no thread neighbours to walk in this grouping; the
    // panel stays open on the anchor.
    await browser.execute(() => document.activeElement?.blur());
    await browser.keys("ArrowDown");
    await browser.waitUntil(async () => await detailOpen(), {
      timeoutMsg: "arrow key closed the detail panel",
    });
    assert.equal(
      await browser.$$("#detail li.thread-row.current").length,
      1,
      "arrow key did not fabricate a neighbour",
    );

    // Esc closes the panel.
    await browser.execute(() => document.activeElement?.blur());
    await browser.keys("Escape");
    await browser.waitUntil(async () => !(await detailOpen()), {
      timeoutMsg: "Escape did not close the detail panel",
    });
  });
}
