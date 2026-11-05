// TRANSIENT test-phase driver (deleted after this validation run).
// Adversarial: the preview root is a foreign-owned (root) 0777 directory
// that the app user (nobody, uid 65534) cannot delete or chmod. The
// permission enforcement must fail closed: media_open_external returns
// an error naming preview permissions BEFORE any copy is written, the
// foreign entry's mode stays untouched, and no preview copy exists.
export const name = "media-foreign-dir-fail-closed";

import { copyFile, mkdir, writeFile, readFile, rm, readdir, stat } from "node:fs/promises";
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

  const previewRoot = "/tmp/mailchat-media-open";

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // Stage a stored media object (the command checks object existence
    // before touching the preview tree, so the store must be seeded).
    const exportDir = path.join(dataDir, "tg-media");
    await mkdir(path.join(exportDir, "voice_messages"), { recursive: true });
    const beep = path.join(mediaFixtures(), "beep.wav");
    await copyFile(beep, path.join(exportDir, "voice_messages/audio_201.ogg"));
    await writeFile(
      path.join(exportDir, "result.json"),
      JSON.stringify({
        name: "ForeignDir",
        type: "private_group",
        id: 15,
        messages: [
          { id: 201, type: "message", date_unixtime: "1700000100", from: "Bo",
            media_type: "voice_message", file: "voice_messages/audio_201.ogg", text: "" },
        ],
      }),
    );
    const outcome = await invoke(browser, "import_chat_export", {
      path: exportDir,
      extractMedia: true,
    });
    assert.ok(outcome.ok, `import succeeded: ${outcome.error ?? ""}`);
    const sha = await sha256Hex(beep);

    // The container planted a root-owned 0777 directory at the preview
    // root before this unprivileged run started; assert it is really
    // the hostile state this scenario needs. When the plant is absent
    // (a plain run of the suite: earlier media specs legitimately end
    // with the preview root removed, and only a root actor can re-plant
    // a foreign-owned directory) this spec skips — same pattern as the
    // notify specs without their bus capture — and the matrix e2e leg
    // runs it in a dedicated pass that plants the hostile root.
    let hostile;
    try {
      hostile = await stat(previewRoot);
    } catch {
      ctx.skip("hostile preview root not planted — needs the e2e container pass that pre-creates a root-owned 0777 " + previewRoot);
    }
    assert.ok(hostile.isDirectory(), "hostile preview root is a directory");
    assert.ok((hostile.mode & 0o777) === 0o777, "hostile preview root is 0777");

    const open = await invoke(browser, "media_open_external", {
      name: `${sha}.ogg`,
      displayName: "audio_201.ogg",
    });
    assert.ok(!open.ok, `external open fails closed (got ${JSON.stringify(open)})`);
    assert.ok(
      /preview permissions/.test(open.error),
      `error names preview permissions (got ${open.error})`,
    );

    // Fail-closed: no copy anywhere, foreign mode untouched.
    const after = await readdir(previewRoot);
    assert.ok(after.length === 0, `no preview copy landed (root holds ${JSON.stringify(after)})`);
    const still = await stat(previewRoot);
    assert.ok((still.mode & 0o777) === 0o777, `foreign dir mode untouched (got ${(still.mode & 0o777).toString(8)})`);

  });
}

function mediaFixtures() {
  return path.resolve(fixturesDir, "..", "..", "..", "crates", "vault-core", "tests", "fixtures", "media");
}
