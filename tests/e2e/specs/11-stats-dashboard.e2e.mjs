// Path: statistika arhive — the statistics dashboard over the archive:
// empty-archive summary, aggregates after demo seed + mbox import, the
// Refresh button, idempotent re-import (counters must not double-count),
// and a counter-vs-table consistency check through the real database.

import { mkdirSync } from "node:fs";

export const name = "stats-dashboard";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) {
    return null;
  }
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    // --- 1. fresh boot, empty archive ------------------------------------
    const stats = await ui.waitForApp(browser);
    assert.equal(stats, "empty archive", "fresh install stats label");
    await ui.openView(browser, "activity");

    const summary = await ui.waitForText(browser, "#dashboard .meta", (text) =>
      text.includes("archive"),
    );
    assert.equal(summary, "empty archive", "empty dashboard summary");
    assert.equal(
      await browser.$("#dashboard .day-chart").isExisting(),
      false,
      "no histogram on an empty archive",
    );
    assert.equal(
      await browser.$("#dashboard .stat-table").isExisting(),
      false,
      "no stat tables on an empty archive",
    );
    await saveScreenshot(browser, "09-dashboard-empty.png");

    // --- 2. seed demo → aggregates appear --------------------------------
    await ui.seedDemo(browser);
    await ui.openView(browser, "activity");
    const seeded = await ui.waitForText(browser, "#dashboard .meta", (text) =>
      text.startsWith("6 messages"),
    );
    assert.matches(
      seeded,
      /^6 messages · 1 source · 2026-11-01 → 2026-11-01$/,
      "dashboard summary after demo seed",
    );
    assert.equal(
      (await browser.$$("#dashboard .day-chart .day-bar").length),
      1,
      "one day bar for the single demo day",
    );
    assert.equal(
      (await browser.$$("#dashboard .stat-table").length),
      3,
      "three stat tables (source, chats, authors)",
    );

    // "Top chats": demo · project-chat 4, demo · welcome 2 (count desc).
    // Label cells are ellipsized, and WebKitDriver returns no visible text
    // for them — read the accessible `title` attribute instead.
    const chatLabel = async (i) =>
      (await browser.$(`#dashboard .stat-table:nth-of-type(2) tbody tr:nth-of-type(${i}) td.label`))
        .getAttribute("title");
    const chatCount = async (i) =>
      (await browser.$(`#dashboard .stat-table:nth-of-type(2) tbody tr:nth-of-type(${i}) td.num`)).getText();
    assert.equal(await chatLabel(1), "demo · project-chat", "top chat label");
    assert.equal(await chatCount(1), "4", "top chat by count");
    assert.equal(await chatLabel(2), "demo · welcome", "second chat label");
    assert.equal(await chatCount(2), "2", "second chat count");

    // "Top authors": four authors, counts 2/2/1/1.
    const authorCount = await browser
      .$$("#dashboard .stat-table:nth-of-type(3) tbody tr")
      .then((rows) => rows.length);
    assert.equal(authorCount, 4, "four demo authors listed");

    // "By source" table leads with demo/6.
    const sourceLabel = await browser
      .$("#dashboard .stat-table:nth-of-type(1) tbody tr td.label").getAttribute("title");
    const sourceCount = await browser
      .$("#dashboard .stat-table:nth-of-type(1) tbody tr td.num").getText();
    assert.equal(sourceLabel, "demo", "by-source label");
    assert.equal(sourceCount, "6", "by-source count for demo");

    await saveScreenshot(browser, "09-dashboard-seeded.png");

    // --- 3. import the mbox fixture → dashboard refreshes automatically --
    const importStatus = await ui.importMbox(browser, ui.archiveMbox);
    assert.matches(importStatus, /Imported 8 new/, "mbox import");
    await ui.openView(browser, "activity");
    const imported = await ui.waitForText(browser, "#dashboard .meta", (text) =>
      text.startsWith("14 messages"),
    );
    assert.matches(imported, /2 sources/, "dashboard now spans two sources");

    // --- 4. Refresh button ------------------------------------------------
    await browser.$("#stats-refresh").click();
    // The refresh must succeed (no error status) and re-render the same data.
    await ui.waitForText(
      browser,
      "#dashboard-status",
      (text) => text === "",
      { timeoutMsg: "dashboard status shows an error after manual refresh" },
    );
    const afterRefresh = await (await browser.$("#dashboard .meta")).getText();
    assert.matches(afterRefresh, /^14 messages · 2 sources/, "dashboard intact after Refresh");

    // --- 5. idempotent re-import: counters must not double-count ---------
    await ui.importMbox(browser, ui.archiveMbox);
    await ui.openView(browser, "activity");
    const afterReimport = await ui.waitForText(browser, "#dashboard .meta", (text) =>
      text.startsWith("14 messages"),
    );
    assert.matches(
      afterReimport,
      /2 sources/,
      "re-import adds nothing; dashboard total unchanged",
    );

    // --- 6. counter tables agree with the message rows --------------------
    if (sqlite) {
      const db = vaultDbPath(dataDir);
      const sums = await ctx.sqlite.query(db, `
        SELECT
          (SELECT COALESCE(SUM(n), 0) FROM stats_source) AS src_total,
          (SELECT COALESCE(SUM(n), 0) FROM stats_day)    AS day_total,
          (SELECT COALESCE(SUM(n), 0) FROM stats_chat)   AS chat_total,
          (SELECT COALESCE(SUM(n), 0) FROM stats_author) AS author_total,
          (SELECT COUNT(*) FROM archive_messages)       AS msg_total
      `);
      const row = sums[0];
      for (const key of ["src_total", "day_total", "chat_total", "author_total"]) {
        assert.equal(row[key], row.msg_total, `stats_${key.split("_")[0]} sums to COUNT(*)`);
      }
    }

    await saveScreenshot(browser, "09-dashboard-full.png");
  });
}
