// Shared UI-driving helpers. The suite must run against both frontend
// generations: the current classic UI and the filtered-search UI
// (fm/mailchat-search-ui), so helpers probe which DOM they are talking to
// and specs assert on the strings both generations share (stats label,
// import status) or branch on `uiMode()`.

import path from "node:path";
import { fileURLToPath } from "node:url";

export const fixturesDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "fixtures",
);
export const archiveMbox = path.join(fixturesDir, "archive.mbox");

/** "classic" (current main) or "filtered" (the search-UI rewrite). */
export async function uiMode(browser) {
  return (await browser.$("#from-date").isExisting()) ? "filtered" : "classic";
}

/** Wait until the app booted: the stats label replaced its placeholder. */
/** Wait until the app booted: the stats label replaced its placeholder.
 * Some WebKitWebDriver builds return an empty string from getText()
 * for live-region elements even when the text is set, so the read
 * falls back to the DOM textContent via execute — the assertion is
 * unchanged: the label must actually hold the boot-time stats text. */
export async function waitForApp(browser, { timeout = 30_000 } = {}) {
  const stats = await browser.$("#stats");
  const read = async () => {
    const text = await stats.getText().catch(() => "");
    if (text !== "") return text;
    return browser.execute(() => document.querySelector("#stats")?.textContent ?? "");
  };
  await browser.waitUntil(
    async () => {
      const text = await read();
      return text !== "" && text !== "…" && !text.startsWith("Loading");
    },
    { timeout, timeoutMsg: "app did not boot (stats label never filled in)" },
  );
  return read();
}

/**
 * Open a navigation destination (workspace | sources | activity |
 * settings) through the real nav buttons and wait for the view to mount.
 * Elements only exist visibly inside their view, so helpers/specs must
 * open the view before reading or clicking view-scoped controls.
 */
export async function openView(browser, view) {
  const app = await browser.$("#app");
  if ((await app.getAttribute("data-view")) === view) return;
  const button = await browser.$(
    `#nav-views .nav-view-btn[data-view-target="${view}"]`,
  );
  await button.click();
  await browser.waitUntil(
    async () => (await app.getAttribute("data-view")) === view,
    { timeout: 5_000, timeoutMsg: `view "${view}" never mounted` },
  );
}

/** Resize the app window (drives the wide/medium/compact layout states). */
export async function setViewport(browser, width, height) {
  await browser.setWindowRect(0, 0, width, height);
  const app = await browser.$("#app");
  await browser.waitUntil(
    async () => {
      const layout = await app.getAttribute("data-layout");
      return layout === resolveLayoutName(width);
    },
    { timeout: 5_000, timeoutMsg: `layout never settled for ${width}x${height}` },
  );
}

function resolveLayoutName(width) {
  if (width >= 1100) return "wide";
  if (width >= 800) return "medium";
  return "compact";
}

/** The current layout state (data-layout on #app). */
export async function layoutMode(browser) {
  return (await browser.$("#app")).getAttribute("data-layout");
}

async function textOf(browser, selector) {
  const el = await browser.$(selector);
  return el.getText().catch(() => "");
}

/** Wait until `selector`'s text satisfies `predicate` (string contains are typical). */
export async function waitForText(browser, selector, predicate, { timeout = 15_000 } = {}) {
  const description =
    typeof predicate === "string" ? `contains ${JSON.stringify(predicate)}` : "matches predicate";
  await browser.waitUntil(
    async () => {
      const text = await textOf(browser, selector);
      if (typeof predicate === "string") {
        return text.includes(predicate);
      }
      return predicate(text);
    },
    { timeout, timeoutMsg: `timeout waiting for ${selector} text to become: ${description}` },
  );
  return textOf(browser, selector);
}

/** Clear an element's text so a settle-wait can only be satisfied by the
 * NEW action's output: the previous action's lingering status satisfies
 * the same predicate, so a stale poll could return before the trigger is
 * even processed (CI flake: stale results list clicked). */
async function clearStatus(browser, selector) {
  await browser.execute((sel) => {
    document.querySelector(sel).textContent = "";
  }, selector);
}

/** Type a path into the import field, click Import, return the final status. */
export async function importMbox(browser, mboxPath) {
  await openView(browser, "sources"); // import controls live in the Add-source view
  const input = await browser.$("#mbox-path");
  await input.setValue(mboxPath);
  await clearStatus(browser, "#import-status");
  const button = await browser.$("#import-btn");
  await button.click();
  // Both UI generations print "Imported N new, M already present (of X in src)."
  return waitForText(
    browser,
    "#import-status",
    (text) => text.includes(" new, ") && !text.startsWith("Importing"),
    { timeoutMsg: "import never finished" },
  );
}

export async function seedDemo(browser) {
  await openView(browser, "sources"); // the demo button lives in the Add-source view
  await clearStatus(browser, "#import-status");
  const button = await browser.$("#seed-btn");
  await button.click();
  return waitForText(browser, "#import-status", "Demo messages:", {
    timeoutMsg: "demo seeding never finished",
  });
}

/** Submit a search (Enter in the query box), return the settled status line. */
export async function search(browser, text) {
  await openView(browser, "workspace"); // search lives in the workspace view
  const input = await browser.$("#search");
  await input.click();
  await input.setValue(text);
  await clearStatus(browser, "#status");
  await browser.keys("Enter");
  return waitForText(
    browser,
    "#status",
    (text) =>
      text !== "" && !text.startsWith("Searching") && !text.startsWith("Type something"),
    { timeout: 15_000, timeoutMsg: "search never settled" },
  );
}

/** All result rows as text (works for both generations' list markup). */
export async function resultTexts(browser) {
  return browser.$$("#results li").map((li) => li.getText());
}

/** Read `#status` once. */
export function statusText(browser) {
  return textOf(browser, "#status");
}

/** Extract the total match count from either generation's status line. */
export function totalFromStatus(status, mode) {
  if (mode === "classic") {
    const match = /^(\d+) results?/.exec(status);
    if (!match) {
      throw new Error(`cannot read total from classic status: ${JSON.stringify(status)}`);
    }
    return Number(match[1]);
  }
  const match = /(\d+)\s*$|of\s+(\d+)/.exec(status);
  const of = /of\s+(\d+)/.exec(status);
  if (of) {
    return Number(of[1]);
  }
  if (status.startsWith("0 results")) {
    return 0;
  }
  throw new Error(`cannot read total from filtered status: ${JSON.stringify(status)}`);
}
