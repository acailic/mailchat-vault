// Path: automatic folder watching — the real app watches a folder, imports
// export files that appear or change (debounced, deduped), reports them in
// the events list, and stops when the folder is removed.

import { writeFile } from "node:fs/promises";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "folder-watch";

function mail(id, subject) {
  return [
    `From alice@example.com Sat Nov  1 10:00:00 2026`,
    `From: Alice Smith <alice@example.com>`,
    `To: Bob <bob@example.com>`,
    `Subject: ${subject}`,
    `Date: Sat, 1 Nov 2026 10:00:00 +0100`,
    `Message-ID: <${id}@watch.local>`,
    ``,
    `body of ${subject}`,
    ``,
  ].join("\n");
}

export async function run(ctx) {
  const { withSession, assert, ui, vaultDbPath } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    await ui.openSourcesSection(browser, "automatic");

    const watched = await mkdtemp(path.join(tmpdir(), "mailchat-watch-"));
    // Two files present before the folder is armed: the initial scan
    // imports them.
    await writeFile(path.join(watched, "first.mbox"), mail("w1", "One") + mail("w2", "Two"));
    // An unrecognized .txt: skipped with a visible reason, never an error.
    await writeFile(path.join(watched, "notes.txt"), "just a shopping list\n");

    // --- watch the folder by typed path -----------------------------------
    // The typed path sits behind the "Type a path instead" fold (the
    // folder picker is the primary control) — open it first.
    await (await browser.$("#watch-typed-fold summary")).click();
    const input = await browser.$("#watch-path");
    await input.setValue(watched);
    await (await browser.$("#watch-btn")).click();
    await ui.waitForText(browser, "#watch-status", "Watching 1 folder", {
      timeoutMsg: "folder never armed",
    });

    // --- initial scan: both mbox messages imported, notes.txt skipped -----
    await ui.waitForText(browser, "#stats", "2 messages (Mail import: 2)", {
      timeout: 30_000,
      timeoutMsg: "initial scan never imported the mbox",
    });
    const events = await (await browser.$("#watch-events")).getText();
    assert.ok(
      events.includes("imported") && events.includes("2 new"),
      `events list shows the import, got: ${events}`,
    );
    assert.ok(events.includes("skipped"), `notes.txt skip is visible, got: ${events}`);

    // --- a new file appearing later is imported too ------------------------
    await writeFile(path.join(watched, "second.mbox"), mail("w3", "Three"));
    await ui.waitForText(browser, "#stats", "3 messages (Mail import: 3)", {
      timeout: 30_000,
      timeoutMsg: "new file never imported",
    });

    // --- re-writing a file imports only what is new (dedup contract) ------
    await writeFile(path.join(watched, "second.mbox"), mail("w3", "Three") + mail("w4", "Four"));
    await ui.waitForText(browser, "#stats", "4 messages (Mail import: 4)", {
      timeout: 30_000,
      timeoutMsg: "modified file never re-imported",
    });
    // The counts stay stable: no duplicates from re-settling.
    await new Promise((r) => setTimeout(r, 4000));
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "4 messages (Mail import: 4)",
      "no duplicates after re-settling",
    );

    // --- direct database confirmation -------------------------------------
    const db = vaultDbPath(dataDir);
    if (ctx.sqlite) {
      const count = await ctx.sqlite.query(
        db,
        "SELECT COUNT(*) AS n FROM archive_messages",
      );
      assert.equal(count[0].n, 4, "row count in canonical archive_messages table");
      const folders = await ctx.sqlite.query(db, "SELECT path FROM watched_folders");
      assert.equal(folders.length, 1, "watched folder persisted");
    }

    // --- removal stops the imports ----------------------------------------
    await (await browser.$("#watch-folders .watch-remove")).click();
    await ui.waitForText(browser, "#watch-status", "No folders watched yet", {
      timeoutMsg: "folder never removed",
    });
    if (ctx.sqlite) {
      // On-disk removal check gated on #271: mid-session writes stop
      // reaching vault.db (the CLI sees a frozen WAL snapshot while the
      // app serves fresh state). The UI-level removal above and the
      // not-imported check below carry the contract until it lands.
      ctx.log("      (on-disk watched_folders check pending #271)");
    }
    await writeFile(path.join(watched, "third.mbox"), mail("w5", "Five"));
    await new Promise((r) => setTimeout(r, 4000));
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "4 messages (Mail import: 4)",
      "file after removal was not imported",
    );
  });
}
