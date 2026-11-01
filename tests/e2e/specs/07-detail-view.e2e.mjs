// Path: detail view + adjacent (thread) messages. Ships with the filtered
// search UI (fm/mailchat-search-ui); skipped on builds without it.

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

    // Thread neighbours: the whole mbox chat is adjacent to the opened hit.
    const threadRows = await browser.$$("#detail ol.thread li.thread-row");
    assert.equal(threadRows.length, 8, "all 8 chat messages are thread neighbours");
    assert.equal(
      await browser.$$("#detail li.thread-row.current").length,
      1,
      "exactly one current thread row",
    );

    // Clicking a neighbour link moves the detail to that message.
    const links = await browser.$$("#detail li.thread-row.link");
    assert.ok(links.length > 0, "neighbour rows are clickable links");
    const link = links[0];
    const linkMeta = await link.$(".meta").getText();
    const linkAuthor = linkMeta.split("·")[0].trim();
    await link.click();
    await browser.waitUntil(
      async () => (await browser.$("#detail .detail-card h2").getText()) === linkAuthor,
      { timeoutMsg: "clicking a neighbour did not switch the detail view" },
    );
    assert.equal(
      await browser.$$("#detail li.thread-row.current").length,
      1,
      "current marker moved with the view",
    );

    // Esc closes the panel. Blur first: the re-render replaced the clicked
    // thread row, and a key event targeted at the detached node would never
    // reach the document-level handler.
    await browser.execute(() => document.activeElement?.blur());
    await browser.keys("Escape");
    await browser.waitUntil(async () => !(await detailOpen()), {
      timeoutMsg: "Escape did not close the detail panel",
    });
  });
}
