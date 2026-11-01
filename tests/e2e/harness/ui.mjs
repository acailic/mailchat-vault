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
export async function waitForApp(browser, { timeout = 30_000 } = {}) {
  const stats = await browser.$("#stats");
  await browser.waitUntil(
    async () => {
      const text = await stats.getText().catch(() => "");
      return text !== "" && text !== "…" && !text.startsWith("Loading");
    },
    { timeout, timeoutMsg: "app did not boot (stats label never filled in)" },
  );
  return stats.getText();
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

/** Type a path into the import field, click Import, return the final status. */
export async function importMbox(browser, mboxPath) {
  const input = await browser.$("#mbox-path");
  await input.setValue(mboxPath);
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
  const button = await browser.$("#seed-btn");
  await button.click();
  return waitForText(browser, "#import-status", "Demo messages:", {
    timeoutMsg: "demo seeding never finished",
  });
}

/** Submit a search (Enter in the query box), return the settled status line. */
export async function search(browser, text) {
  const input = await browser.$("#search");
  await input.click();
  await input.setValue(text);
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
