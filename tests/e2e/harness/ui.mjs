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

/** Invoke a Tauri command inside the live webview (real IPC, real app)
 * and resolve with its value; rejects with the command's error string.
 * (The same seam the backup/discord specs use for native-dialog-bound
 * flows: the command runs with the path the picker would have returned.) */
export async function invokeCommand(browser, cmd, args = {}) {
  await browser.execute(
    (command, payload) => {
      window.__e2eInvoke = null;
      window.__e2eInvokeError = null;
      window.__TAURI_INTERNALS__.invoke(command, payload)
        .then((value) => {
          window.__e2eInvoke = { ok: value };
        })
        .catch((error) => {
          window.__e2eInvokeError = String(error);
        });
    },
    cmd,
    args,
  );
  return browser.waitUntil(
    async () =>
      (await browser.execute(() => window.__e2eInvoke !== null || window.__e2eInvokeError !== null)),
    { timeout: 60_000, timeoutMsg: `${cmd} never settled` },
  ).then(async () => {
    const { ok, error } = await browser.execute(() => ({
      ok: window.__e2eInvoke?.ok,
      error: window.__e2eInvokeError,
    }));
    if (error !== null && error !== undefined) throw new Error(`${cmd} failed in webview: ${error}`);
    return ok;
  });
}

/** Poll a submitted job to a terminal state; returns the job record. */
export async function waitForJob(browser, id, { timeout = 120_000 } = {}) {
  let job = null;
  await browser.waitUntil(
    async () => {
      job = await invokeCommand(browser, "job_get", { id });
      return ["succeeded", "failed", "cancelled"].includes(job?.state?.state);
    },
    { timeout, timeoutMsg: `job ${id} never reached a terminal state` },
  );
  return job;
}

/** Read #stats via textContent — the same WebKitWebDriver getText()
 * empty-string quirk waitForApp works around (live-region elements). */
async function statsLabelText(browser) {
  return browser.execute(
    () => document.querySelector("#stats")?.textContent ?? "",
  );
}

/** Wait until the #stats label matches a fresh `stats` round trip — the
 * deterministic settle for "the UI noticed the archive changed" (the
 * terminal job event refreshes every vault-reading panel). */
export async function waitForStatsSync(browser, { timeout = 30_000 } = {}) {
  await browser.waitUntil(
    async () => {
      const stats = await invokeCommand(browser, "stats", {});
      const label = await statsLabelText(browser);
      const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
      const expected =
        stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
      return label === expected;
    },
    { timeout, timeoutMsg: "stats label never synced with the vault" },
  );
  return statsLabelText(browser);
}

/** Import an .mbox file into the running app through the v2 job engine
 * (the same bounded importer the UI's confirm step dispatches to; the
 * native picker itself is beyond WebDriver's reach, as documented). The
 * UI picks the change up through the job-event stream; resolves with the
 * run's ingest summary: { source, seen, inserted, skipped, failed }. */
export async function importMbox(browser, mboxPath) {
  const { id } = await invokeCommand(browser, "job_submit", {
    kind: "import_mbox",
    params: { path: mboxPath },
  });
  const job = await waitForJob(browser, id);
  if (job.state.state !== "succeeded") {
    throw new Error(`import_mbox job ended ${job.state.state}: ${job.state.error?.message}`);
  }
  // import_mbox jobs report the summary directly; chat jobs wrap it.
  const result = job.state.result ?? {};
  const summary = result.summary ?? result;
  await waitForStatsSync(browser);
  return summary;
}

export async function seedDemo(browser) {
  // Through the job engine (like importMbox): the terminal job event
  // is what refreshes the UI's panels.
  const { id } = await invokeCommand(browser, "job_submit", {
    kind: "seed_demo",
    params: {},
  });
  const job = await waitForJob(browser, id);
  if (job.state.state !== "succeeded") {
    throw new Error(`seed_demo job ended ${job.state.state}: ${job.state.error?.message}`);
  }
  const summary = job.state.result ?? {};
  await waitForStatsSync(browser);
  return `Demo messages: ${summary.inserted} new, ${summary.skipped} already present.`;
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
