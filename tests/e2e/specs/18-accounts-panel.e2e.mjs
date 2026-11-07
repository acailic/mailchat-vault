// Path: the multi-account UI (roadmap: the accounts panel) an end user
// sees — the Accounts view of the live app: the empty state, the listing
// after a real mbox import (import accounts appear with per-account
// stats), an IMAP account's identity + credential probe (seeded directly
// into the live vault DB the same way 14-imap-coordinator does), sync
// now with no stored credential surfacing the backend's guidance to the
// user, pause/resume with the flag persisted in the vault DB, forget
// credentials when nothing is stored (the honest "had no stored
// credentials" answer), and the keyboard path — every action is a real
// focusable button whose aria-label names the account.
//
// The container has no TLS-trustable IMAP server the app's
// rustls/webpki-roots stack can trust (the same honest limitation spec
// 14 records), so a sync-now that actually fetches mail is covered by
// vault-core's mock-server suites (tests/accounts.rs); what this spec
// proves on the live product is the accounts surface itself.
//
// Heads-up on ordering: the app refreshes the accounts panel on boot,
// after imports, and after job events, so the seeded row needs a click
// on Refresh only when the seed happens after the last auto-refresh.

import { mkdirSync } from "node:fs";

export const name = "accounts-panel";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) {
    return null;
  }
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

/** SQL to insert a disposable IMAP source row into the live vault DB
 * (accounts_list enumerates source_instances). The config mirrors what
 * `ensure_source_instance` stores for a mail server source. */
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
    // 1. Empty state: a fresh vault lists no accounts and shows the
    //    guidance line instead of an empty panel.
    // =============================================================
    await ui.openView(browser, "accounts");
    const empty = await ui.waitForText(
      browser,
      "#accounts-list",
      "No accounts yet",
      { timeoutMsg: "the empty-state line never appeared" },
    );
    assert.includes(empty, "connect an email account (IMAP) or import a chat/mbox export", "the empty state guides the user");
    await browser.execute(() =>
      document.querySelector("#accounts-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "18-accounts-empty.png");

    // =============================================================
    // 2. A real mbox import shows up as an import account with its
    //    per-account stats.
    // =============================================================
    await ui.importMbox(browser, ui.archiveMbox);
    await ui.openView(browser, "accounts");
    const listText = await ui.waitForText(
      browser,
      "#accounts-list",
      "Import",
      { timeoutMsg: "the import account never appeared in the panel" },
    );
    assert.includes(listText, "8 messages", "the import account's message count");
    assert.includes(listText, "imported", "the import account's sync state");
    const status = await ui.waitForText(browser, "#accounts-status", "1 account · 1 import", {
      timeoutMsg: "the panel status never summarized the listing",
    });
    assert.equal(status, "1 account · 1 import", "the status line summarizes kinds");
    await browser.execute(() =>
      document.querySelector("#accounts-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "18-accounts-after-import.png");

    // =============================================================
    // 3. An IMAP account (seeded into the live vault DB) lists with
    //    its connection identity and the honest no-credentials probe.
    // =============================================================
    await ctx.sqlite.query(db, sourceRow(
      "e2e-imap-credless",
      "imap:mail.test:993:credless@h.invalid",
      "credless@h.invalid",
    ));
    await (await browser.$("#accounts-refresh")).click();
    const imapRow = await ui.waitForText(
      browser,
      "#accounts-list",
      "credless@h.invalid",
      { timeoutMsg: "the IMAP account never appeared in the panel" },
    );
    assert.includes(imapRow, "Email (IMAP)", "the account's kind");
    assert.includes(imapRow, "no credentials stored", "the honest credential probe");
    assert.includes(imapRow, "never synced", "the never-synced stamp");

    // =============================================================
    // 4. Sync now with nothing stored: the backend's guidance reaches
    //    the user (sync_imap ran with the stored identity + empty
    //    password, and its error is what the panel reports).
    // =============================================================
    const syncBtn = (await browser.$$(".account-sync-btn"))[0];
    assert.matches(syncBtn ? await syncBtn.getAttribute("aria-label") : "", /credless@h\.invalid/, "the action names its account for screen readers");
    await browser.execute(() =>
      document.querySelector(".account-sync-btn")?.scrollIntoView({ block: "center" }),
    );
    // Drive the action through the keyboard path (focus + Enter), the
    // way a keyboard-only user would.
    await browser.execute(() => document.querySelector(".account-sync-btn")?.focus());
    await browser.keys("Enter");
    const syncErr = await ui.waitForText(
      browser,
      "#accounts-status",
      "no password stored for this account",
      { timeoutMsg: "the sync-now guidance never surfaced" },
    );
    assert.includes(syncErr, "Sync failed for imap:mail.test:993:credless@h.invalid", "the failure names the account");
    await browser.execute(() =>
      document.querySelector("#accounts-status")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "18-accounts-sync-now-no-credentials.png");

    // =============================================================
    // 5. Pause/resume: the exclusion flag persists in the vault DB and
    //    the row's sync state says paused.
    // =============================================================
    await browser.execute(() => document.querySelector(".account-pause-btn")?.focus());
    await browser.keys("Enter");
    await ui.waitForText(browser, "#accounts-list", "paused — auto-sync skips this account", {
      timeoutMsg: "the row never showed the paused state",
    });
    const pausedRows = await ctx.sqlite.query(
      db,
      `SELECT excluded FROM source_instances WHERE id = 'e2e-imap-credless'`,
    );
    assert.equal(pausedRows[0].excluded, 1, "the exclusion persisted");
    await browser.execute(() =>
      document.querySelector("#accounts-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "18-accounts-paused.png");

    await browser.execute(() => document.querySelector(".account-pause-btn")?.focus());
    await browser.keys("Enter");
    await ui.waitForText(browser, "#accounts-list", "connected · never synced", {
      timeoutMsg: "the row never showed the resumed state",
    });
    const resumedRows = await ctx.sqlite.query(
      db,
      `SELECT excluded FROM source_instances WHERE id = 'e2e-imap-credless'`,
    );
    assert.equal(resumedRows[0].excluded, 0, "the resume cleared the flag");

    // =============================================================
    // 6. Forget credentials with nothing stored: the command answers
    //    honestly (nothing was stored) instead of claiming success.
    // =============================================================
    await browser.execute(() => document.querySelector(".account-forget-btn")?.focus());
    await browser.keys("Enter");
    const forgetLine = await ui.waitForText(
      browser,
      "#accounts-status",
      "had no stored credentials",
      { timeoutMsg: "the forget-credentials answer never surfaced" },
    );
    assert.includes(forgetLine, "credless@h.invalid", "the answer names the account");

    // The archive-side stats still sum to what the source chips show
    // (reuse-stats contract) — the panel adds no new storage.
    const chips = await ui.waitForText(browser, "#stats", "8", {
      timeoutMsg: "the archive stats label never filled",
    });
    assert.includes(chips, "8", "the archive-wide stats still report the import");
    log("accounts panel: all scenarios passed");
  });
}
