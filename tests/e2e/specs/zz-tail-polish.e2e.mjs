// Path: the tail-polish pass (§2.6) — every surface a user reads speaks
// in user words, never raw store tokens: the reading pane's date and
// conversation certainty rows, the Copy-message text, the backend text
// export, the accounts panel identity (address · server, tooltip with
// the stored source label, actions that name the account), and the
// "Add account…" arrival context on the Add source view.
//
// The unknown-token defense (verbatim passthrough) has no live surface —
// the store's CHECK constraint on time_confidence makes an unknown token
// unrepresentable in a live vault — so it is covered by the mirrored unit
// tests (src/format.test.ts, crates/vault-core/src/search2.rs).

import { readFileSync } from "node:fs";
import { mkdirSync } from "node:fs";

export const name = "tail-polish";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return null;
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

/** SQL to insert a disposable IMAP source row into the live vault DB
 * (accounts_list enumerates source_instances) — the same seeding spec 18
 * uses, so the row reads exactly like one the app itself would store. */
function sourceRow(id, label, username) {
  const config = JSON.stringify({
    host: "mail.test",
    port: 993,
    tls_mode: "implicit-tls",
    username,
    folders: ["INBOX"],
  });
  return `INSERT INTO source_instances (id, kind, label, connector_key, config_json, state, excluded)
          VALUES ('${id}', 'imap', '${label}', 'imap|mail.test|993|implicit-tls|${username}', '${config}', 'connected', 0)`;
}

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath, log } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    if (!sqlite) {
      throw new Error("sqlite3 CLI unavailable — this spec needs direct DB asserts");
    }
    const db = vaultDbPath(dataDir);

    // =============================================================
    // 1. Reading pane: mbox messages carry `explicit_offset`, so the
    //    date row must read "(exact)" with a tooltip explaining what
    //    the archive knows — never the raw token.
    // =============================================================
    await ui.importMbox(browser, ui.archiveMbox);
    const status = await ui.search(browser, "vault");
    assert.equal(ui.totalFromStatus(status, "filtered"), 4, "four vault hits");
    const hits = await browser.$$("#results li.hit-item");
    await hits[0].click();
    await browser.waitUntil(
      async () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "clicking a hit did not open the detail panel" },
    );

    const dateRow = await browser.execute(() => {
      const dd = Array.from(document.querySelectorAll("#detail .meta-table dt"))
        .find((dt) => dt.textContent === "date")?.nextElementSibling;
      return dd ? { text: dd.textContent, title: dd.title } : null;
    });
    assert.ok(dateRow, "the date meta row rendered");
    assert.includes(dateRow.text, "(exact)", "the date row speaks user words, not explicit_offset");
    assert.ok(!dateRow.text.includes("explicit_offset"), "raw token never reaches the pane");
    assert.includes(dateRow.title, "Time certainty: exact", "tooltip names the certainty");
    assert.includes(dateRow.title, "read from the message itself", "tooltip explains the basis");

    // =============================================================
    // 2. Conversation row: the mbox fixture is an unverified grouping
    //    — "(unverified grouping)" plus the honest tooltip; the raw
    //    legacy token never reaches the row.
    // =============================================================
    const convRow = await browser.execute(() => {
      const dd = Array.from(document.querySelectorAll("#detail .meta-table dt"))
        .find((dt) => dt.textContent === "conversation")?.nextElementSibling;
      return dd ? { text: dd.textContent, title: dd.title } : null;
    });
    assert.ok(convRow, "the conversation meta row rendered");
    assert.includes(convRow.text, "(unverified grouping)", "the grouping honesty is spoken, not legacy_unresolved");
    assert.ok(!convRow.text.includes("legacy_unresolved"), "raw token never reaches the row");
    assert.includes(convRow.title, "The source did not confirm", "the unverified tooltip explains the limit");

    // The meta table values, taken together, hold no raw token.
    const metaText = await browser.execute(
      () => document.querySelector("#detail .meta-table")?.textContent ?? "",
    );
    assert.ok(!metaText.includes("explicit_offset"), "no raw time token in the meta table");
    assert.ok(!metaText.includes("legacy_unresolved"), "no raw conversation token in the meta table");
    await browser.execute(() =>
      document.querySelector("#detail")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "zz-tail-reading-pane.png");

    // =============================================================
    // 3. Copy message: the clipboard text the real handler writes
    //    carries the same user wording (captured via a page-level
    //    clipboard stub — WebKitWebDriver cannot read the clipboard).
    // =============================================================
    await browser.execute(() => {
      window.__copied = null;
      const target = navigator.clipboard;
      Object.defineProperty(target, "writeText", {
        configurable: true,
        value: (text) => {
          window.__copied = text;
          return Promise.resolve();
        },
      });
    });
    await (await browser.$("#detail .copy-btn")).click();
    await browser.waitUntil(
      async () => (await browser.execute(() => window.__copied)) !== null,
      { timeoutMsg: "the copy handler never wrote the clipboard" },
    );
    const copied = await browser.execute(() => window.__copied);
    assert.includes(copied, "(exact)", "copy text speaks the time certainty");
    assert.includes(copied, "(unverified grouping)", "copy text speaks the grouping honesty");
    assert.ok(!copied.includes("explicit_offset"), "raw time token never copied");
    assert.ok(!copied.includes("legacy_unresolved"), "raw conversation token never copied");

    // =============================================================
    // 4. Backend text export: the real export_messages_v2 command
    //    writes detail_as_text — the same user wording on the Rust
    //    side, proven by the file the product actually wrote.
    // =============================================================
    const idRows = await ctx.sqlite.query(
      db,
      `SELECT id FROM archive_messages ORDER BY id LIMIT 1`,
    );
    const exportPath = evidenceDir
      ? `${evidenceDir}/zz-tail-selection-export.txt`
      : "/tmp/zz-tail-selection-export.txt";
    const summary = await ui.invokeCommand(browser, "export_messages_v2", {
      ids: [idRows[0].id],
      format: "text",
      path: exportPath,
    });
    assert.equal(summary.written, 1, "the export wrote the message");
    const exportText = readFileSync(exportPath, "utf8");
    assert.includes(exportText, "(exact)", "export speaks the time certainty");
    assert.includes(exportText, "(unverified grouping)", "export speaks the grouping honesty");
    assert.ok(!exportText.includes("explicit_offset"), "raw time token never exported");
    assert.ok(!exportText.includes("legacy_unresolved"), "raw conversation token never exported");
    assert.ok(!exportText.includes("()"), "no empty qualifier fragment in the export");

    // =============================================================
    // 5. Accounts panel: an IMAP account is named "address · server",
    //    the stored connector label survives only as a tooltip, and
    //    the per-row actions name the account the user knows.
    // =============================================================
    await ctx.sqlite.query(db, sourceRow(
      "e2e-imap-tail",
      "imap:mail.test:993:credless@h.invalid",
      "credless@h.invalid",
    ));
    await ui.openView(browser, "accounts");
    await (await browser.$("#accounts-refresh")).click();
    const rowText = await ui.waitForText(
      browser,
      "#accounts-list",
      "credless@h.invalid · mail.test",
      { timeoutMsg: "the IMAP row never showed the address · server identity" },
    );
    assert.includes(rowText, "credless@h.invalid · mail.test", "the title is address · server");
    assert.ok(
      !rowText.includes("imap:mail.test:993"),
      "the raw connector label is not the row title",
    );
    const tooltip = await browser.execute(() => {
      const el = document.querySelector('.account-row[data-source-id="e2e-imap-tail"] .account-label');
      return el ? el.title : null;
    });
    assert.equal(
      tooltip,
      "Stored source: imap:mail.test:993:credless@h.invalid",
      "the stored label survives as the row's tooltip",
    );
    const syncAria = await browser.execute(() => {
      const row = document.querySelector('.account-row[data-source-id="e2e-imap-tail"]');
      const btn = row?.querySelector(".account-sync-btn");
      return btn ? btn.getAttribute("aria-label") : null;
    });
    assert.equal(
      syncAria,
      "Sync account credless@h.invalid · mail.test now",
      "the sync action names the account the user knows",
    );
    await browser.execute(() =>
      document.querySelector("#accounts-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "zz-tail-accounts-row.png");

    // =============================================================
    // 6. "Add account…" arrives on Add source WITH context, and the
    //    guidance retires when the user leaves the view on their own.
    // =============================================================
    await (await browser.$("#accounts-add")).click();
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => document.querySelector("#app")?.getAttribute("data-view"))) ===
        "sources",
      { timeoutMsg: "Add account never navigated to the sources view" },
    );
    const guidance = await ui.waitForText(
      browser,
      "#add-source-guidance",
      "Import files",
      { timeoutMsg: "the arrival guidance never appeared" },
    );
    assert.includes(guidance, "Connect email (IMAP)", "the guidance names the connect path");
    assert.includes(guidance, "Accounts panel", "the guidance says where the result shows up");
    await browser.execute(() =>
      document.querySelector("#add-source-guidance")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "zz-tail-add-source-guidance.png");

    // Self-directed revisit: the guidance served its purpose and must
    // not linger.
    await ui.openView(browser, "workspace");
    await ui.openView(browser, "sources");
    const hidden = await browser.execute(
      () => document.querySelector("#add-source-guidance")?.hidden,
    );
    assert.equal(hidden, true, "the guidance retired once the user left the view");

    log("tail polish: all scenarios passed");
  });
}
