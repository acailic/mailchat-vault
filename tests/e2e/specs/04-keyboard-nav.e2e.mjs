// Path: keyboard navigation. Enter submits in both UI generations; the
// filtered UI additionally ships /, ↑/↓ result selection, Enter to open the
// detail view and Esc to close it.

export const name = "keyboard-nav";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    await ui.importMbox(browser, ui.archiveMbox);
    const mode = await ui.uiMode(browser);

    // Enter in the query box submits the search (no button click).
    const input = await browser.$("#search");
    await input.click();
    await input.setValue("zephyr");
    await browser.keys("Enter");
    const status = await ui.waitForText(
      browser,
      "#status",
      (text) => text !== "" && !text.startsWith("Searching") && !text.startsWith("Type"),
      { timeoutMsg: "Enter-submitted search never settled" },
    );
    assert.equal(ui.totalFromStatus(status, mode), 1, "Enter submitted the search");

    if (mode !== "filtered") {
      return; // arrow-key navigation ships with the filtered UI only
    }

    // --- filtered UI: /, arrows, Enter, Esc -------------------------------
    // Keyboard nav acts on document-level keys while focus is OUTSIDE text
    // inputs, so every step starts by parking focus on a non-interactive
    // element (the h1 header — clicking "body" would hit a result row and
    // open the detail view).
    const parkFocus = async () => {
      await browser.$("h1").click();
      await browser.waitUntil(
        async () =>
          (await browser.execute(() => document.activeElement?.id ?? "")) !== "search",
        { timeout: 3000, timeoutMsg: "parking focus never blurred the search input" },
      );
    };

    await input.setValue("vault");
    await browser.keys("Enter");
    await ui.waitForText(
      browser,
      "#status",
      (text) => text.includes("of 4"),
      { timeoutMsg: "vault search never settled" },
    );

    // "/" focuses the search box from anywhere outside an input.
    await parkFocus();
    await browser.keys("/");
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => document.activeElement?.id ?? "")) === "search",
      { timeout: 3000, timeoutMsg: "/ focuses the search box" },
    );

    // Arrow keys walk the result list; Enter opens the detail view.
    await parkFocus();
    const activeIndex = () =>
      browser.$("#results li.hit-item.active").getAttribute("data-index");
    await browser.keys("ArrowDown");
    await browser.waitUntil(async () => (await activeIndex()) === "0", {
      timeoutMsg: "first ArrowDown selects the first hit",
    });
    await browser.keys("ArrowDown");
    await browser.waitUntil(async () => (await activeIndex()) === "1", {
      timeoutMsg: "second ArrowDown moves down",
    });
    await browser.keys("ArrowUp");
    await browser.waitUntil(async () => (await activeIndex()) === "0", {
      timeoutMsg: "ArrowUp moves back up",
    });

    // Esc with no detail open just clears the selection.
    await browser.keys("Escape");
    await browser.waitUntil(async () => (await browser.$$("#results li.hit-item.active")).length === 0, {
      timeoutMsg: "Escape clears the selection",
    });

    // Re-select and open the detail view with Enter.
    await browser.keys("ArrowDown");
    await browser.keys("Enter");
    const detailOpen = () =>
      browser.execute(() => document.querySelector("#detail.open") !== null);

    // Re-select and open the detail view with Enter.
    await browser.keys("ArrowDown");
    await browser.keys("Enter");
    await browser.waitUntil(async () => await detailOpen(), {
      timeoutMsg: "Enter did not open the detail view",
    });
    // WebKit's WebDriver serves stale matches for findElement on this
    // selector after the class flips, so state checks go through page JS.
    await browser.keys("Escape");
    await browser.waitUntil(async () => !(await detailOpen()), {
      timeoutMsg: "Escape did not close the detail view",
    });
  });
}
