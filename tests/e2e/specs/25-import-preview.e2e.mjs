// Path: U03 — the bounded import preview, driven over real IPC against
// the live app (the native picker itself is beyond WebDriver's reach, the
// documented limitation; the preview/confirm contract is the backend
// surface the picker hands its result to — vault-core unit tests cover
// the scan semantics, this spec proves them on the real binary).

import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "import-preview";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // --- .mbox: format, source, bounded exact counts, samples, size ------
    const mbox = await ui.invokeCommand(browser, "preview_import", {
      path: ui.archiveMbox,
    });
    assert.equal(mbox.kind, "mbox");
    assert.equal(mbox.source, "mbox");
    assert.equal(mbox.messages_seen, 8, "all 8 fixture mails seen");
    assert.equal(mbox.scan_complete, true, "small files scan completely");
    assert.equal(mbox.parse_failures, 0);
    assert.equal(mbox.samples.length, 3, "samples are capped at three");
    assert.ok(mbox.samples[0].author.length > 0, "samples carry authors");
    assert.ok(mbox.input_bytes > 0, "input size measured");
    assert.ok(mbox.free_bytes > 0, "free space on the vault disk reported");

    // --- Signal conversation export ---------------------------------------
    const signal = await ui.invokeCommand(browser, "preview_import", {
      path: path.join(ui.fixturesDir, "signal-conversation.json"),
    });
    assert.equal(signal.kind, "signal");
    assert.ok(signal.messages_seen > 0, "the signal export has messages");

    // --- Discord Data Package directory pick -------------------------------
    const discord = await ui.invokeCommand(browser, "preview_import", {
      path: path.join(ui.fixturesDir, "discord-package"),
    });
    assert.equal(discord.kind, "discord");
    assert.ok(discord.messages_seen > 0, "the discord package has messages");

    // --- Telegram export with missing media: absence is provable -----------
    const dir = mkdtempSync(path.join(tmpdir(), "mc25-"));
    const telegramPath = path.join(dir, "result.json");
    writeFileSync(
      telegramPath,
      JSON.stringify({
        name: "Arhiva",
        id: 42,
        messages: [
          {
            id: 1,
            type: "message",
            date: "2026-11-01T10:00:00",
            from: "Marko",
            text: "look",
            photo: "photos/gone.jpg",
          },
        ],
      }),
    );
    const telegram = await ui.invokeCommand(browser, "preview_import", {
      path: telegramPath,
    });
    assert.equal(telegram.kind, "telegram");
    assert.equal(telegram.media_referenced, 1, "the photo reference is detected");
    assert.equal(telegram.media_missing, 1, "the absent photo is reported before import");

    // --- unsupported and missing inputs: guidance, never raw errno ---------
    const notes = path.join(dir, "notes.txt");
    writeFileSync(notes, "just some notes with no export shape at all\n");
    const unsupported = await ui
      .invokeCommand(browser, "preview_import", { path: notes })
      .then(() => null, (e) => String(e));
    assert.ok(
      unsupported !== null && /unrecognized/i.test(unsupported),
      `unsupported input explains itself (got: ${unsupported})`,
    );

    const missing = await ui
      .invokeCommand(browser, "preview_import", { path: path.join(dir, "nope.mbox") })
      .then(() => null, (e) => String(e));
    assert.ok(
      missing !== null && /not found/i.test(missing),
      `a missing file says not found, not errno (got: ${missing})`,
    );

    // --- nothing was stored by any preview ---------------------------------
    const stats = await ui.invokeCommand(browser, "stats", {});
    assert.equal(stats.total, 0, "previews store nothing");
  });
}
