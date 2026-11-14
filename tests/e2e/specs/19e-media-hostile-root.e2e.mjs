// TRANSIENT test-phase driver (deleted after this validation run).
// Adversarial: a hostile local co-user pre-plants a symlink (or any
// non-directory entry) at the app's preview root before an external
// open. The purge must remove the planted entry itself, recreate the
// app-owned private tree, and the copy must land there — never inside
// the attacker-controlled target.
export const name = "media-hostile-preview-root";

import { copyFile, mkdir, writeFile, readFile, rm, symlink, readdir, stat, lstat } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { fixturesDir } from "../harness/ui.mjs";

const sha256Hex = async (p) =>
  createHash("sha256").update(await readFile(p)).digest("hex");

async function invoke(browser, command, args = {}) {
  try {
    return {
      ok: true,
      value: await browser.execute(
        async (cmd, payload) => window.__TAURI_INTERNALS__.invoke(cmd, payload),
        command,
        args,
      ),
    };
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  const attackerDir = "/tmp/mc-attacker-target";
  const previewRoot = "/tmp/mailchat-media-open";

  const fakeBin = "/tmp/mc-fake-handler-19e/bin";
  await rm("/tmp/mc-fake-handler-19e", { recursive: true, force: true });
  await mkdir(fakeBin, { recursive: true });
  const seen19e = "/tmp/mc-fake-handler-19e/seen.txt";
  await writeFile(
    path.join(fakeBin, "fake-browser"),
    `#!/bin/sh\necho "$@" >> ${seen19e}\n`,
    { mode: 0o755 },
  );

  await withSession({ env: { BROWSER: path.join(fakeBin, "fake-browser") } }, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // Stage a stored media object the same way the dispatch spec does.
    const exportDir = path.join(dataDir, "tg-media");
    await mkdir(path.join(exportDir, "voice_messages"), { recursive: true });
    const beep = path.join(mediaFixtures(), "beep.wav");
    await copyFile(beep, path.join(exportDir, "voice_messages/audio_201.ogg"));
    await writeFile(
      path.join(exportDir, "result.json"),
      JSON.stringify({
        name: "HostileRoot",
        type: "private_group",
        id: 14,
        messages: [
          { id: 201, type: "message", date_unixtime: "1700000100", from: "Bo",
            media_type: "voice_message", file: "voice_messages/audio_201.ogg", text: "" },
        ],
      }),
    );
    const outcome = await ui.importChatExport(browser, exportDir, { extractMedia: true });
    assert.ok(outcome.ok, `import succeeded: ${outcome.error ?? ""}`);
    const sha = await sha256Hex(beep);

    // Plant the hostile state: a decoy-bearing attacker dir reached
    // through a symlink at the preview root path.
    await rm(attackerDir, { recursive: true, force: true });
    await rm(previewRoot, { recursive: true, force: true });
    await mkdir(attackerDir, { recursive: true });
    await writeFile(path.join(attackerDir, "decoy.txt"), "attacker decoy");
    await symlink(attackerDir, previewRoot);

    const open = await invoke(browser, "media_open_external", {
      name: `${sha}.ogg`,
      displayName: "audio_201.ogg",
    });
    assert.ok(open.ok, `external open succeeds (got ${JSON.stringify(open)})`);
    assert.ok(
      open.value.path.startsWith(path.join(previewRoot, sha)),
      `copy stays in the app-owned preview tree (got ${open.value.path})`,
    );

    // The planted target received nothing new, and the root path is now
    // a real app-owned directory, not the attacker's link anymore.
    const attackerEntries = await readdir(attackerDir);
    assert.ok(
      attackerEntries.length === 1 && attackerEntries[0] === "decoy.txt",
      `attacker dir holds only its own decoy (got ${JSON.stringify(attackerEntries)})`,
    );
    const rootMeta = await lstat(previewRoot);
    assert.ok(rootMeta.isDirectory(), "preview root is a real directory, symlink removed");
    const rootMode = rootMeta.mode & 0o777;
    assert.ok(rootMode === 0o700, `preview root enforced 0700 (got ${rootMode.toString(8)})`);
    const copyMeta = await stat(open.value.path);
    const copyMode = copyMeta.mode & 0o777;
    assert.ok(copyMode === 0o600, `copy enforced 0600 (got ${copyMode.toString(8)})`);
    assert.ok(
      Buffer.compare(await readFile(open.value.path), await readFile(beep)) === 0,
      "copy is byte-exact",
    );

    await rm(attackerDir, { recursive: true, force: true });
    await rm(previewRoot, { recursive: true, force: true });
    await rm("/tmp/mc-fake-handler-19e", { recursive: true, force: true });
  });
}

function mediaFixtures() {
  return path.resolve(fixturesDir, "..", "..", "..", "crates", "vault-core", "tests", "fixtures", "media");
}
