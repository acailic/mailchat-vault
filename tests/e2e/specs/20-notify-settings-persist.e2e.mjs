// Path: the persisted Settings option behind the notification layer,
// driven at the live product surface:
//   - an older settings.json written before the `notifications` option
//     existed (field absent) loads with the option ENABLED: the Settings
//     checkbox is checked on boot — the documented backfill default;
//   - toggling the checkbox off and saving persists `"notifications":
//     false` to the settings file;
//   - a fresh app boot over that file starts with the checkbox unchecked
//     (the opt-out survives restart);
//   - re-enabling persists `"notifications": true` again.
//
// The settings file lives in the app data dir (XDG_DATA_HOME/<app id>),
// asserted directly as the persisted-state contract.

import { mkdtemp, mkdir, writeFile, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "notify-settings-persist";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;
const APP_ID = "com.acailic.mailchatvault";

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return null;
  await mkdir(evidenceDir, { recursive: true });
  try {
    await browser.saveScreenshot(`${evidenceDir}/${filename}`);
  } catch {
    // bonus
  }
  return `${evidenceDir}/${filename}`;
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  // A pre-option settings file: the current version, providers empty,
  // options without the new field — exactly what an upgrading user has.
  const dataDir = await mkdtemp(path.join(tmpdir(), "mailchat-notify-persist-"));
  const appDir = path.join(dataDir, APP_ID);
  await mkdir(appDir, { recursive: true });
  const settingsPath = path.join(appDir, "settings.json");
  const olderFile = {
    version: 1,
    providers: [],
    default_provider: null,
    options: { theme: "system", summarize_top: 8 },
  };
  await writeFile(settingsPath, JSON.stringify(olderFile));

  // --- boot 1: the absent field backfills to enabled ---------------------
  await withSession({ dataDir }, async (browser) => {
    await ui.waitForApp(browser);
    await ui.openSettingsSection(browser, "general");
    const toggle = await browser.$("#notify-toggle");
    await browser.waitUntil(async () => {
      try {
        return await toggle.isSelected();
      } catch {
        return false;
      }
    }, { timeoutMsg: "the Desktop notifications checkbox never mounted" });
    assert.ok(
      await toggle.isSelected(),
      "old settings file backfills the option to enabled",
    );
    await saveScreenshot(browser, "20-backfill-checked.png");

    // Opt out and save: the file must record the explicit opt-out. The
    // save confirmation lives on the AI page (hidden while General is up;
    // WebKit getText of a hidden element is ""), so the settle reads the
    // app's own note through the DOM.
    await toggle.click();
    await (await browser.$("#settings-save-general")).click();
    await browser.waitUntil(
      async () =>
        (await browser.execute(
          () => document.querySelector("#settings-note")?.textContent ?? "",
        )).startsWith("Saved."),
      { timeoutMsg: "the opt-out save never confirmed" },
    );
    const saved = JSON.parse(await readFile(settingsPath, "utf8"));
    assert.equal(
      saved.options.notifications,
      false,
      "opt-out persisted to settings.json",
    );
    // keepData: without it the harness deletes this shared data dir at
    // session end, and boot 2 would restart over nothing — the backfill
    // default would re-enable the option and the restart assert would
    // read the shipped `checked` checkbox, not the persisted opt-out.
  }, { keepData: true });

  // --- boot 2: the opt-out survives restart ------------------------------
  await withSession({ dataDir }, async (browser) => {
    await ui.waitForApp(browser);
    await ui.openSettingsSection(browser, "general");
    // The checkbox ships `checked` in the HTML and is corrected only when
    // the settings round-trip renders; wait for that render (the
    // summarize-top input is empty until then) before reading the toggle,
    // or the assert races the boot and flakes.
    await browser.waitUntil(async () => {
      try {
        return (await (await browser.$("#summarize-top")).getValue()) !== "";
      } catch {
        return false;
      }
    }, { timeout: 15_000, timeoutMsg: "settings never rendered (summarize-top stayed empty)" });
    const toggle = await browser.$("#notify-toggle");
    assert.ok(
      !(await toggle.isSelected()),
      "the opt-out survives an app restart",
    );

    // Re-enable and save: true persists again.
    await toggle.click();
    await (await browser.$("#settings-save-general")).click();
    await browser.waitUntil(
      async () =>
        (await browser.execute(
          () => document.querySelector("#settings-note")?.textContent ?? "",
        )).startsWith("Saved."),
      { timeoutMsg: "the re-enable save never confirmed" },
    );
    const saved = JSON.parse(await readFile(settingsPath, "utf8"));
    assert.equal(saved.options.notifications, true, "re-enable persisted");
    await saveScreenshot(browser, "20-re-enabled.png");
  });
}
