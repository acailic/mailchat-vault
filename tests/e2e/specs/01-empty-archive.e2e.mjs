// Path: prazna baza / empty archive — first boot state before any import.

import { existsSync } from "node:fs";

export const name = "empty-archive";

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    const stats = await ui.waitForApp(browser);
    assert.equal(stats, "empty archive", "fresh install stats label");

    // Without an API key the LLM note must say so up front (classic UI).
    if (await browser.$("#llm-note").isExisting()) {
      const note = await browser.$("#llm-note").getText();
      assert.ok(
        note.startsWith("LLM assist is off"),
        `llm note should announce LLM-off mode, got: ${note}`,
      );
    }

    // Searching the empty archive is a clean zero-result state, not an error.
    const status = await ui.search(browser, "vault");
    assert.matches(status, /^0 results/, "empty-archive search status");
    assert.equal((await ui.resultTexts(browser)).length, 0, "no result rows");

    // U02: an empty archive presents the first-source journey, not a
    // dead search prompt. A blank query keeps that state honestly.
    const firstRun = await browser.$("#first-run");
    assert.ok(await firstRun.isDisplayed(), "the first-run card is visible on an empty archive");
    const input = await browser.$("#search");
    await input.click();
    await input.setValue("   ");
    await browser.keys("Enter");
    await ui.waitForText(browser, "#status", "Your archive is empty");
    assert.ok(await firstRun.isDisplayed(), "the first-run card survives a blank query");
    const banner = await browser.$("#demo-banner");
    assert.equal(await banner.isDisplayed(), false, "no demo banner outside the sample archive");

    // The database was created on boot and holds nothing.
    const db = vaultDbPath(dataDir);
    assert.ok(existsSync(db), `vault.db created at ${db}`);
    if (ctx.sqlite) {
      const rows = await ctx.sqlite.query(db, "SELECT COUNT(*) AS n FROM archive_messages");
      assert.equal(rows[0].n, 0, "canonical archive_messages table is empty");
    }
  });
}
