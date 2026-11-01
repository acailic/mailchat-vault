// Path: date-range filtering through the UI date inputs, centred on the
// near-midnight boundary. Ships with the filtered search UI
// (fm/mailchat-search-ui); skipped on builds without date inputs. The
// near-midnight case is additionally covered headlessly on every build via
// the LLM Ask spec (LLM-planned date range over the same fixture).

export const name = "date-range";

const settleStatus = (browser, ui) =>
  ui.waitForText(
    browser,
    "#status",
    (text) => !text.startsWith("Searching") && text !== "" && !text.startsWith("Type"),
    { timeoutMsg: "date-filtered browse never settled" },
  );

async function setDate(browser, selector, value) {
  await browser.execute(
    (el, next) => {
      el.value = next;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    await browser.$(selector),
    value,
  );
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;
  const settle = (browser) => settleStatus(browser, ui);
  // The fixture mails straddle *UTC* midnight (23:59:30Z / 00:00:30Z); the
  // date inputs filter by *local* day. Only under TZ=UTC do the two mails
  // sit on different local dates, so pin the app's timezone the same way
  // the llm_search fixtures do.
  await withSession({ env: { TZ: "UTC" } }, async (browser) => {
    await ui.waitForApp(browser);
    if ((await ui.uiMode(browser)) !== "filtered") {
      ctx.skip("date-range UI pending (fm/mailchat-search-ui)");
    }

    await ui.importMbox(browser, ui.archiveMbox);

    // Empty text + date filter = browse under that filter.
    const searchInput = await browser.$("#search");
    await searchInput.click();
    await searchInput.clearValue();

    // One day only: 2025-10-31 contains exactly the 23:59:30 message.
    await setDate(browser, "#from-date", "2025-10-31");
    await setDate(browser, "#to-date", "2025-10-31");
    let status = await settle(browser);
    assert.equal(
      ui.totalFromStatus(status, "filtered"),
      1,
      "Oct 31 alone matches only the before-midnight mail",
    );
    let rows = await ui.resultTexts(browser);
    assert.ok(rows[0].includes("before midnight"), "the 23:59:30 mail is shown");

    // Extending the upper bound to Nov 1 pulls in the 00:00:30 mail — the
    // boundary must be inclusive through midnight.
    await setDate(browser, "#to-date", "2025-11-01");
    status = await settle(browser);
    assert.equal(ui.totalFromStatus(status, "filtered"), 2, "through Nov 1 matches the pair");
    rows = await ui.resultTexts(browser);
    assert.ok(
      rows.some((r) => r.includes("before midnight")) &&
        rows.some((r) => r.includes("after midnight")),
      "both sides of midnight are inside the range",
    );

    // Nov 1 alone: only the after-midnight mail.
    await setDate(browser, "#from-date", "2025-11-01");
    status = await settle(browser);
    assert.equal(ui.totalFromStatus(status, "filtered"), 1, "Nov 1 alone matches 00:00:30 only");
    rows = await ui.resultTexts(browser);
    assert.ok(rows[0].includes("after midnight"), "the 00:00:30 mail is shown");

    // Wide range: 7 dated messages; the undated mail stays excluded while a
    // bound is active because it cannot be placed in time.
    await setDate(browser, "#from-date", "2025-10-01");
    await setDate(browser, "#to-date", "2025-12-31");
    status = await settle(browser);
    assert.equal(
      ui.totalFromStatus(status, "filtered"),
      7,
      "wide range matches all dated messages, excluding the undated one",
    );

    // Clear returns to the unfiltered prompt (empty query, no filters).
    await browser.$("#clear-filters").click();
    await ui.waitForText(browser, "#status", "Type something to search");
  });
}
