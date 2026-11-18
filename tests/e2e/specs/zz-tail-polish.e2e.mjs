// Path: the tail-polish pass (§2.6) — every surface a user reads speaks
// in user words, never raw store tokens: the reading pane's date and
// conversation certainty rows, the Copy-message text, the backend text
// export, the sources list identity (address · server, tooltip with
// the stored source label, actions that name the source), and the
// Add source chooser round trip on the Sources view.
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
    // 5. Sources list: an IMAP source is named "address · server",
    //    the stored connector label survives only as a tooltip, and
    //    the per-row actions name the source the user knows.
    // =============================================================
    await ctx.sqlite.query(db, sourceRow(
      "e2e-imap-tail",
      "imap:mail.test:993:credless@h.invalid",
      "credless@h.invalid",
    ));
    // The row lists with the user-known identity: "username · host"
    // as the title, the stored connector label only as the tooltip,
    // and the per-row actions aria-named after that title (the WAL
    // guard keeps the app's readers and the CLI seed on one epoch —
    // #271 — so the refresh shows the row).
    await ui.openView(browser, "sources");
    await (await browser.$("#sources-refresh")).click();
    const tailRow = await ui.waitForText(
      browser,
      "#sources-list",
      "credless@h.invalid · mail.test",
      { timeoutMsg: "the seeded IMAP row never appeared with its identity title" },
    );
    assert.ok(
      !tailRow.includes("imap:mail.test:993"),
      "the raw connector key is not the row title",
    );
    const tailAria = await browser.execute(() => {
      const btn = document.querySelector("#sources-list .account-sync-btn");
      return btn?.getAttribute("aria-label") ?? "";
    });
    assert.includes(
      tailAria,
      "credless@h.invalid",
      "the row action names the source the user knows",
    );
    await browser.execute(() =>
      document.querySelector("#sources-list")?.scrollIntoView({ block: "center" }),
    );

    // =============================================================
    // 6. The Add source chooser: exactly two ways in (Import files /
    //    Connect email), the email form carries visible labels, and
    //    Back returns to the sources home.
    // =============================================================
    await (await browser.$("#sources-add-btn")).click().catch(async () => {
      // Driver click can refuse on a freshly re-rendered view (stale
      // snapshot class) — the live DOM click is the fallback.
      await browser.execute(() => document.querySelector("#sources-add-btn")?.click());
    });
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => document.querySelector("#app")?.getAttribute("data-sources-section"))) ===
        "add",
      { timeoutMsg: "Add source never opened the chooser" },
    );
    const choices = await browser.$$("#sources-add .row button:not([id='add-back-btn'])");
    const choiceLabels = [];
    for (const choice of choices) choiceLabels.push(await choice.getText());
    assert.deepEqual(
      choiceLabels.map((t) => t.trim()),
      ["Import files…", "Connect email…"],
      "the chooser offers exactly the two ways in",
    );
    await (await browser.$("#add-choice-email")).click();
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => document.querySelector("#app")?.getAttribute("data-sources-section"))) ===
        "email",
      { timeoutMsg: "Connect email never opened the email form" },
    );
    // Visible field labels (not placeholders doing the labelling).
    for (const labelFor of ["imap-user", "imap-auth", "imap-pass"]) {
      assert.ok(
        await (await browser.$(`#sources-email label[for="${labelFor}"]`)).isDisplayed(),
        `the ${labelFor} field carries a visible label`,
      );
    }
    // Server/port/folders wait under Advanced, not in the primary flow.
    assert.equal(
      await browser.execute(() => document.querySelector("#imap-advanced")?.open ?? null),
      false,
      "the Advanced fold starts closed",
    );
    await browser.execute(() =>
      document.querySelector("#sources-email")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "zz-tail-add-source-email.png");

    // Back returns home; the chooser is gone and the list is back.
    await (await browser.$("#email-back-btn")).click();
    await browser.waitUntil(
      async () =>
        (await browser.execute(() => document.querySelector("#app")?.getAttribute("data-sources-section"))) ===
        "home",
      { timeoutMsg: "Back never returned to the sources home" },
    );
    assert.ok(
      await (await browser.$("#sources-add-btn")).isDisplayed(),
      "the home section (with its Add source button) is back",
    );

    log("tail polish: all scenarios passed");
  });
}
