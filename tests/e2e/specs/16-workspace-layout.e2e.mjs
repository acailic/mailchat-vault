// Path: M3.1 workspace layout. Three areas (navigation / results /
// reading) with the wide/medium/compact narrow-window states, the compact
// navigation drawer, the 200%-zoom equivalence (a halved viewport must go
// compact, never clip horizontally), the semantic landmarks/live regions,
// and the wide/medium/compact/zoom screenshot fixtures (synthetic demo
// data only) saved into tests/e2e/fixtures/screenshots/.
//
// Drawer visibility is probed through page JS (computed visibility +
// bounding rect): WebKitGTK's visibility checks ignore transforms, and the
// compact resize can take a moment to settle before the first probe.

import { mkdirSync } from "node:fs";
import path from "node:path";

export const name = "workspace-layout";

const shotsDir = path.resolve(
  path.dirname(new URL(import.meta.url).pathname),
  "..",
  "fixtures",
  "screenshots",
);

/** Save a screenshot fixture into the repo (best effort). */
async function fixtureShot(browser, name) {
  try {
    mkdirSync(shotsDir, { recursive: true });
    await browser.saveScreenshot(path.join(shotsDir, name));
  } catch {
    // Screenshots are evidence, never a failure cause.
  }
}

/** No horizontal clipping: the page never scrolls sideways. */
async function noHorizontalClip(browser) {
  return browser.execute(
    () =>
      document.documentElement.scrollWidth <=
      document.documentElement.clientWidth + 1,
  );
}

/** Is the compact navigation drawer actually shown (visible and on-screen)? */
async function drawerShown(browser) {
  return browser.execute(() => {
    const nav = document.querySelector("#nav");
    if (!nav) return false;
    const cs = getComputedStyle(nav);
    const rect = nav.getBoundingClientRect();
    return cs.visibility !== "hidden" && cs.display !== "none" && rect.width > 0 && rect.right > 0;
  });
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // --- semantic landmarks, labels, live regions -------------------------
    const semantics = await browser.execute(() => {
      const unnamed = [...document.querySelectorAll("button")]
        .filter(
          (b) =>
            !b.textContent.trim() &&
            !b.getAttribute("aria-label") &&
            !b.getAttribute("title"),
        )
        .map((b) => b.id || b.className);
      return {
        nav: !!document.querySelector('nav#nav[aria-label="Workspace"]'),
        main: !!document.querySelector("main#main"),
        reading: !!document.querySelector('aside#reading[aria-label="Reading"]'),
        statusRole: document.querySelector("#status")?.getAttribute("role"),
        statusLive: document.querySelector("#status")?.getAttribute("aria-live"),
        statsRole: document.querySelector("#stats")?.getAttribute("role"),
        searchLabelled: !!document.querySelector('label[for="search"]'),
        h1Count: document.querySelectorAll("h1").length,
        unnamedButtons: unnamed,
        skipLink: !!document.querySelector("a.skip-link[href='#main']"),
      };
    });
    assert.ok(semantics.nav, "navigation landmark with label");
    assert.ok(semantics.main, "main landmark");
    assert.ok(semantics.reading, "reading landmark with label");
    assert.equal(semantics.statusRole, "status", "results status is a live region");
    assert.equal(semantics.statusLive, "polite", "live region is polite");
    assert.equal(semantics.statsRole, "status", "stats line announces changes");
    assert.ok(semantics.searchLabelled, "the search box has a label");
    assert.equal(semantics.h1Count, 1, "exactly one page heading");
    assert.equal(
      semantics.unnamedButtons.length,
      0,
      `every button has an accessible name (unnamed: ${semantics.unnamedButtons.join(", ")})`,
    );
    assert.ok(semantics.skipLink, "skip link present");

    // --- synthetic data for the visuals -----------------------------------
    await ui.seedDemo(browser);
    await ui.search(browser, "vault");

    // --- wide (>= 1100px): navigation + results + reading side by side ----
    await ui.setViewport(browser, 1280, 800);
    assert.equal(await ui.layoutMode(browser), "wide", "1280px is the wide layout");
    assert.ok(
      await (await browser.$("#nav")).isDisplayed(),
      "navigation column visible in wide",
    );
    assert.ok(
      await (await browser.$("#results-col")).isDisplayed(),
      "results area visible in wide",
    );
    assert.ok(
      await (await browser.$("#reading-empty")).isDisplayed(),
      "reading placeholder shown before a message opens",
    );

    // Open the first hit: the reading pane fills, stays beside the results.
    const hits = await browser.$$("#results .hit-item");
    assert.ok(hits.length > 0, "search returned result rows");
    await hits[0].click();
    await browser.waitUntil(
      async () => (await browser.$("#detail.open")).isExisting(),
      { timeout: 5_000, timeoutMsg: "message never opened in the reading pane" },
    );
    assert.ok(
      await (await browser.$("#results-col")).isDisplayed(),
      "results stay visible while reading in wide",
    );
    const highlights = await browser.$$("#results mark.hit");
    assert.ok(highlights.length > 0, "backend hit markers highlighted");
    await fixtureShot(browser, "16-wide.png");

    // --- medium (800–1099px): reading replaces the results area -----------
    await ui.setViewport(browser, 900, 700);
    assert.equal(await ui.layoutMode(browser), "medium", "900px is the medium layout");
    assert.ok(
      !(await (await browser.$("#results-col")).isDisplayed()),
      "reading replaces the results area in medium",
    );
    const back = await browser.$("#back-to-results");
    assert.ok(await back.isDisplayed(), "Back to results is visible in medium");
    assert.ok(
      await (await browser.$("#detail.open")).isDisplayed(),
      "reading pane visible in medium",
    );
    await fixtureShot(browser, "16-medium.png");

    // Back restores the results with query and rows intact.
    await back.click();
    await browser.waitUntil(
      async () => (await (await browser.$("#results-col"))).isDisplayed(),
      { timeout: 5_000, timeoutMsg: "Back never restored the results area" },
    );
    assert.equal(
      await (await browser.$("#search")).getValue(),
      "vault",
      "Back preserves the query",
    );
    const rowsAfterBack = await browser.$$("#results li.hit-item");
    assert.ok(rowsAfterBack.length > 0, "result rows preserved on Back");

    // --- compact (< 800px): one area + navigation drawer -------------------
    await ui.setViewport(browser, 700, 700);
    // Let the compact relayout (drawer goes off-canvas) settle before probing.
    await new Promise((resolve) => setTimeout(resolve, 1500));
    assert.equal(await ui.layoutMode(browser), "compact", "700px is compact");
    assert.ok(
      await (await browser.$("#nav-toggle")).isDisplayed(),
      "navigation toggle visible",
    );
    assert.equal(await drawerShown(browser), false, "navigation drawer starts closed");
    const toggleState = () => browser.$("#nav-toggle").getAttribute("aria-expanded");
    assert.equal(await toggleState(), "false", "toggle announces collapsed");
    await (await browser.$("#nav-toggle")).click();
    await browser.waitUntil(async () => (await drawerShown(browser)) === true, {
      timeout: 5_000,
      timeoutMsg: "navigation drawer never opened",
    });
    assert.equal(await toggleState(), "true", "toggle announces expanded");
    assert.ok(await noHorizontalClip(browser), "compact never clips horizontally");
    await fixtureShot(browser, "16-compact.png");

    // Escape closes the drawer (04-keyboard-nav covers the reading pane).
    await browser.keys("Escape");
    await browser.waitUntil(async () => (await drawerShown(browser)) === false, {
      timeout: 5_000,
      timeoutMsg: "Escape never closed the navigation drawer",
    });
    assert.equal(await toggleState(), "false", "toggle announces collapsed again");

    // --- 200% zoom equivalence: a halved 1280px viewport goes compact ------
    await ui.setViewport(browser, 640, 480);
    await new Promise((resolve) => setTimeout(resolve, 1000));
    assert.equal(
      await ui.layoutMode(browser),
      "compact",
      "640px (1280 at 200% zoom) is compact",
    );
    assert.ok(
      await noHorizontalClip(browser),
      "no horizontal scrolling at the zoom-equivalent viewport",
    );
    await fixtureShot(browser, "16-compact-zoom.png");

    // --- activity statistics keep their accessible chart -------------------
    await ui.setViewport(browser, 1280, 800);
    await ui.openView(browser, "activity");
    const chart = await browser.$("#dashboard .day-chart");
    assert.ok(await chart.isExisting(), "day histogram rendered for demo data");
    assert.equal(await chart.getAttribute("role"), "img", "histogram is role=img");
    assert.ok(
      (await chart.getAttribute("aria-label"))?.includes("histogram"),
      "histogram carries a label for screen readers",
    );
  });
}
