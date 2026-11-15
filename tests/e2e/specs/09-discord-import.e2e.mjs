// Path: end-to-end Discord Data Package import — the real job engine,
// the real vault database, the real search UI.
//
// The chat-import button opens a native file dialog no WebDriver can
// drive, so the spec submits the very same `import_chat_export` job
// the button's confirm step submits (`job_submit` through the page's
// Tauri IPC bridge, `window.__TAURI_INTERNALS__`) — everything else is
// the user path: job → format detection → vault-core DiscordSource →
// SQLite/FTS5 → search_messages → rendered results.

import path from "node:path";
import { fileURLToPath } from "node:url";

export const name = "discord-import";

const fixturesDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "fixtures",
  "discord-package",
);

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    // Deterministic viewport: the filters row must fit so Clear is
    // clickable regardless of the driver's default window size.
    await browser.setWindowRect(0, 0, 1280, 960).catch(() => {});

    // --- import the whole package directory through the confirm path ----
    const outcome = await ui.importChatExport(browser, fixturesDir);
    assert.ok(outcome.ok, `import job succeeded: ${outcome.error ?? ""}`);
    // 8 user messages across two guild channels and one DM; the
    // GuildMemberJoin system entry is skipped (and not counted as seen).
    const summary = outcome.value.summary;
    assert.equal(summary.source, "discord", "summary source");
    assert.equal(summary.seen, 8, "summary seen");
    assert.equal(summary.inserted, 8, "summary inserted");
    assert.equal(summary.skipped, 0, "summary skipped");
    assert.equal(summary.failed, 0, "summary failed");

    // The job's terminal event already refreshed every vault-reading
    // panel (the helper settles the stats label); Clear refreshes
    // stats too, so the assertion also holds through a real UI action.
    await (await browser.$("#clear-filters")).click();
    assert.equal(
      await ui.waitForText(browser, "#stats", "8 messages (discord: 8)"),
      "8 messages (discord: 8)",
      "stats label after import",
    );

    // --- the imported chat is searchable through the real UI -----------
    const status = await ui.search(browser, "sastanak");
    assert.includes(status, "1–1 of 1", "search result count");
    const results = await ui.resultTexts(browser);
    assert.equal(results.length, 1, "one result row");
    assert.includes(results[0], "petak", "result shows the matching body");

    // --- detail view: author and chat grouping --------------------------
    // Opened with the keyboard UX (park focus, ArrowDown, Enter): the
    // WebKit driver's strict click-intercept check balks at result rows
    // whose center point is a <mark> highlight child.
    await (await browser.$("h1")).click();
    await browser.keys("ArrowDown");
    await browser.keys("Enter");
    const detail = await ui.waitForText(
      browser,
      "#detail",
      (text) => text.includes("ana") && text.includes("discord"),
      { timeoutMsg: "detail never showed the discord message" },
    );
    assert.includes(detail, "Ekipa HQ/general", "detail names the chat");
    await browser.keys("Escape"); // close the panel: it overlays the filters

    // --- DM import resolved its partner via the package index ----------
    const dm = await ui.search(browser, "needle-zzz");
    assert.includes(dm, "1–1 of 1", "DM search result count");

    // --- re-import is idempotent end to end -----------------------------
    const reimport = await ui.importChatExport(browser, fixturesDir);
    assert.ok(reimport.ok, `re-import job succeeded: ${reimport.error ?? ""}`);
    assert.equal(reimport.value.summary.inserted, 0, "re-import inserts nothing");
    assert.equal(reimport.value.summary.skipped, 8, "re-import skips every message");
    await (await browser.$("#clear-filters")).click();
    assert.equal(
      await ui.waitForText(browser, "#stats", "8 messages (discord: 8)"),
      "8 messages (discord: 8)",
      "stats unchanged after re-import",
    );

    // --- the same message id in another channel is no duplicate ---------
    const channelFile = await ui.importChatExport(
      browser,
      path.join(fixturesDir, "messages", "900000000000000101.json"),
    );
    assert.ok(channelFile.ok, `channel-file job succeeded: ${channelFile.error ?? ""}`);
    assert.equal(channelFile.value.summary.inserted, 0, "single channel file re-import dedups");

    // --- database state: grouping, ids, reactions/attachments in raw ----
    const db = vaultDbPath(dataDir);
    if (!ctx.sqlite) {
      ctx.log("      (sqlite3 CLI unavailable — skipping direct DB assertions)");
      return;
    }
    const chats = await ctx.sqlite.query(
      db,
      "SELECT chat_id, COUNT(*) AS n FROM archive_messages GROUP BY chat_id ORDER BY chat_id",
    );
    assert.equal(
      JSON.stringify(chats.map((c) => [c.chat_id, c.n])),
      JSON.stringify([
        ["Direct Messages/@ana", 2],
        ["Ekipa HQ/general", 4],
        ["Ekipa HQ/random", 2],
      ]),
      "chat grouping (DM via index.json, guild channels by name)",
    );

    const ids = await ctx.sqlite.query(
      db,
      "SELECT external_id FROM archive_messages WHERE chat_id = 'Ekipa HQ/general' ORDER BY external_id LIMIT 1",
    );
    assert.equal(
      ids[0].external_id,
      "900000000000000101/1700000000000000001",
      "external_id is channel id / message snowflake",
    );

    const raw = await ctx.sqlite.query(
      db,
      "SELECT raw_json FROM archive_messages WHERE external_id = '900000000000000101/1700000000000000003'",
    );
    const parsed = JSON.parse(raw[0].raw_json);
    assert.equal(parsed.reactions[0].count, 3, "reaction count kept in raw_json");
    assert.equal(parsed.reactions[0].emoji.name, "🔥", "reaction emoji kept in raw_json");
    assert.equal(parsed.attachments[0].filename, "plan.txt", "attachment info kept in raw_json");

    const sentAt = await ctx.sqlite.query(
      db,
      "SELECT sent_instant FROM archive_messages WHERE external_id = '900000000000000101/1700000000000000001'",
    );
    assert.equal(
      sentAt[0].sent_instant,
      "2026-10-31T18:00:05Z",
      "millisecond UTC timestamp normalized to whole seconds",
    );

    // FTS5 index kept in sync by triggers — terms from all three chats.
    // (The sqlite helper takes no bind parameters, so the fixed literals
    // are embedded as FTS5-quoted strings.)
    for (const term of ["kava", "needle-zzz", "petak"]) {
      const fts = await ctx.sqlite.query(
        db,
        `SELECT COUNT(*) AS n FROM archive_fts WHERE archive_fts MATCH '"${term}"'`,
      );
      assert.equal(fts[0].n, 1, `FTS index has ${term}`);
    }
  });
}
