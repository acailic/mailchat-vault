// TRANSIENT test-phase driver (deleted after this validation run).
// The last leg of external open: media_open_external must actually
// dispatch the preview copy to the OS handler. In this headless
// container there is no desktop handler, so a fake `$BROWSER` script
// stands in for the OS: it records the URL it was handed, and the spec
// asserts the command returns success with the copy path it passed to
// the handler and that the handler received the copy's file URL.
export const name = "media-dispatch";

import { copyFile, mkdir, writeFile, readFile, rm } from "node:fs/promises";
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

  // The fake OS handler: records every URL argument it receives.
  const fakeBin = "/tmp/mc-fake-handler/bin";
  const seen = "/tmp/mc-fake-handler/seen.txt";
  await rm(fakeBin, { recursive: true, force: true });
  await mkdir(fakeBin, { recursive: true });
  await writeFile(
    path.join(fakeBin, "fake-browser"),
    "#!/bin/sh\necho \"$@\" >> /tmp/mc-fake-handler/seen.txt\n",
    { mode: 0o755 },
  );

  await withSession(
    { env: { BROWSER: "/tmp/mc-fake-handler/bin/fake-browser" } },
    async (browser, { dataDir }) => {
      await ui.waitForApp(browser);

      const exportDir = path.join(dataDir, "tg-media");
      await mkdir(path.join(exportDir, "voice_messages"), { recursive: true });
      const beep = path.join(mediaFixtures(), "beep.wav");
      await copyFile(beep, path.join(exportDir, "voice_messages/audio_201.ogg"));
      await writeFile(
        path.join(exportDir, "result.json"),
        JSON.stringify({
          name: "Dispatch",
          type: "private_group",
          id: 13,
          messages: [
            { id: 201, type: "message", date_unixtime: "1700000100", from: "Bo",
              media_type: "voice_message", file: "voice_messages/audio_201.ogg", text: "" },
          ],
        }),
      );
      const outcome = await ui.importChatExport(browser, exportDir, { extractMedia: true });
      assert.ok(outcome.ok, `import succeeded: ${outcome.error ?? ""}`);
      const sha = await sha256Hex(beep);

      const open = await invoke(browser, "media_open_external", {
        name: `${sha}.ogg`,
        displayName: "audio_201.ogg",
      });
      assert.ok(open.ok, `external open succeeds end-to-end (got ${JSON.stringify(open)})`);
      assert.ok(
        open.value.path.includes("mailchat-media-open"),
        `command returns the preview copy path (got ${open.value.path})`,
      );
      // The handoff spawns the handler detached — poll for its record.
      let handed = "";
      for (let i = 0; i < 30 && !handed; i++) {
        handed = await readFile(seen, "utf8").catch(() => "");
        if (!handed) await new Promise((r) => setTimeout(r, 100));
      }
      assert.ok(
        handed.includes(open.value.path),
        `OS handler received the preview copy path (seen: ${JSON.stringify(handed)})`,
      );
      const copyBytes = await readFile(open.value.path);
      assert.ok(
        Buffer.compare(copyBytes, await readFile(beep)) === 0,
        "the copy handed to the OS handler is byte-exact",
      );
      const { stat } = await import("node:fs/promises");
      const copyMode = (await stat(open.value.path)).mode & 0o777;
      assert.ok(copyMode === 0o600, `hand-off copy is 0600 (got ${copyMode.toString(8)})`);
      // tidy shared container /tmp
      await rm(fakeBin, { recursive: true, force: true });
    },
  );
}

function mediaFixtures() {
  return path.resolve(fixturesDir, "..", "..", "..", "crates", "vault-core", "tests", "fixtures", "media");
}
