// The Settings General page must show which build is running: the
// version readout (#app-version) is filled at boot from the app_version
// IPC — the version the running binary was stamped with (tauri.conf.json
// version, baked in by tauri-build) — never a frontend constant. Fails
// quiet: hidden when the version cannot be read.
//
// Asserts the readout against the version in src-tauri/tauri.conf.json,
// so a packaged build and a dev build each report their own stamped
// version, and a hardcoded frontend string would fail (it could only
// pass by accident while the config version matches).

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const name = "settings-version-readout";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const conf = JSON.parse(
  readFileSync(path.join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"),
);
const stamped = conf.version;

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return;
  // Scroll the readout into view first: the default viewport shot misses
  // below-the-fold controls.
  await browser.execute(() => {
    document.querySelector("#app-version")?.scrollIntoView({ block: "center" });
  });
  await browser.saveScreenshot(`${evidenceDir}/${filename}`);
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    await ui.openSettingsSection(browser, "general");

    const readout = await browser.$("#app-version");
    assert.ok(
      await readout.isDisplayed(),
      "version readout is visible on the Settings General page",
    );

    // The readout must carry the version the RUNNING BINARY was stamped
    // with — read the textContent via execute (getText() on live-region/
    // meta elements can return "" on this WebKit stack).
    const text = await browser.execute(
      () => document.querySelector("#app-version")?.textContent ?? "",
    );
    assert.equal(
      text,
      `Version ${stamped}`,
      `readout shows the running build's version (expected "Version ${stamped}")`,
    );

    await saveScreenshot(browser, "32-settings-version.png");
  });
}
