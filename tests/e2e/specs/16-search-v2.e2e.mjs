// Path: M3.2 v2 search UI — flat stable ranking, attachment/import-scope
// filter chips, sort choice, deep links, and the restorable results
// selection. Runs through the real v2 IPC (search_archive_v2 et al).

export const name = "search-v2";

const settle = (browser, ui) =>
  ui.waitForText(
    browser,
    "#status",
    (text) => !text.startsWith("Searching") && text !== "" && !text.startsWith("Type"),
    { timeoutMsg: "v2 search never settled" },
  );

/** Set a <select>'s value and dispatch the change event, like typing. */
async function setSelect(browser, selector, value) {
  await browser.execute(
    (el, next) => {
      el.value = next;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    await browser.$(selector),
    value,
  );
}

/** The non-empty option values of a <select>. */
async function optionValues(browser, selector) {
  return browser.execute(
    (el) => [...el.querySelectorAll("option")].map((o) => o.value).filter((v) => v !== ""),
    await browser.$(selector),
  );
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;
  const waitSettled = (browser) => settle(browser, ui);

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("v2 search UI pending (M3.2)");
    }

    await ui.importMbox(browser, ui.archiveMbox);

    // --- flat stable ranking: newest sort orders by instant, id ties ----
    await ui.search(browser, "vault");
    let rows = await browser.$$("#results li.hit-item");
    assert.equal(rows.length, 4, "vault hits rendered flat");
    assert.equal(
      await browser.$$("#results .group").length,
      0,
      "results are a flat list, not regrouped by conversation",
    );

    await setSelect(browser, "#sort-order", "newest");
    await waitSettled(browser);
    const firstId = await (
      (await browser.$$("#results li.hit-item"))[0]
    ).getAttribute("data-id");
    assert.equal(firstId, "4", "newest sort puts the latest dated vault mail first (m4)");
    // And the default order for text is relevance (stable across runs).
    await setSelect(browser, "#sort-order", "relevance");
    const relevanceStatus = await waitSettled(browser);
    assert.equal(ui.totalFromStatus(relevanceStatus, "filtered"), 4, "same count, both sorts");

    // --- attachment filter: the fixture carries no preserved media ------
    // Empty text + the attachment filter = a filtered browse.
    const searchBox = await browser.$("#search");
    await searchBox.click();
    await searchBox.clearValue();
    await setSelect(browser, "#has-attachment", "with");
    const withFiles = await waitSettled(browser);
    assert.equal(
      ui.totalFromStatus(withFiles, "filtered"),
      0,
      "no mbox fixture message references an attachment",
    );
    const chipsWith = await browser.$$("#active-filter-chips .filter-chip");
    assert.ok(
      (await chipsWith[0].getText()).includes("has attachment"),
      "the attachment filter renders as a removable chip",
    );
    // Removing the only chip returns to the unfiltered browse (U04:
    // empty text browses newest-first — 1–8 of 8, no query echo).
    await chipsWith[0].click();
    await ui.waitForText(browser, "#status", (text) => /^1–8 of 8$/.test(text), {
      timeoutMsg: "removing the last filter did not return to the browse",
    });

    // --- import scope: one chip per bounded import run -------------------
    const scopes = await optionValues(browser, "#import-scope");
    assert.ok(scopes.length >= 1, `import runs are listed (${scopes.length} scope(s))`);
    await setSelect(browser, "#import-scope", scopes[0]);
    const scoped = await waitSettled(browser);
    assert.equal(
      ui.totalFromStatus(scoped, "filtered"),
      8,
      "the mbox import scope holds exactly the 8 fixture messages",
    );

    // Removing the chip (with no text) returns to the unfiltered browse.
    const scopeChips = await browser.$$("#active-filter-chips .filter-chip");
    assert.ok(
      (await scopeChips[0].getText()).startsWith("import:"),
      "the import scope renders as a removable chip",
    );
    await scopeChips[0].click();
    await ui.waitForText(browser, "#status", (text) => /^1–8 of 8$/.test(text), {
      timeoutMsg: "removing the import-scope chip did not return to the browse",
    });

    // --- deep link: a pasted locator opens that message -------------------
    const input = await browser.$("#search");
    await input.click();
    await input.setValue("mailchat://message/2");
    await browser.keys("Enter");
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "pasting a message locator did not open the reading pane" },
    );
    const title = await browser.$("#detail .detail-card h2").getText();
    assert.includes(title, "Marko", "the locator opened message 2 (Marko Midnight)");
    // A locator is navigation, not a text query: no search ran, and the
    // status line still shows the empty-prompt state.
    const statusAfterLink = await browser.$("#status").getText();
    assert.ok(
      /^1–8 of 8$/.test(statusAfterLink),
      `no text search ran for the locator, got: ${statusAfterLink}`,
    );

    // Back leaves a usable (empty) results state after a fresh deep link.
    await (await browser.$("#detail .close-btn")).click();
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") === null),
      { timeoutMsg: "Back did not close the reading pane" },
    );
    assert.equal(
      (await browser.$$("#results li.hit-item")).length,
      8,
      "Back restores the unfiltered browse rows, nothing fabricated",
    );
  });
}
