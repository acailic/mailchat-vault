// Path: the M7 background keep-alive at the live product surface:
//   - a settings file with `keep_running_in_background: true` boots with
//     the Settings checkbox checked (the boot-time application of the
//     saved option — the HTML ships it unchecked);
//   - a window close request through the real window API (the same
//     CloseRequested path the window manager's close button takes) hides
//     the window instead of exiting;
//   - the app process itself is provably alive: the main app binary's
//     PID (matched by the session's XDG_DATA_HOME in its environment)
//     still exists under /proc after the close.
//
// Everything runs under Xvfb (the CI e2e job's environment): no tray
// host is needed for the assertions — the appindicator library the e2e
// runners install (alongside the dev stack) is what lets the tray icon
// build (registering with whatever StatusNotifier host it finds — none
// here, where the icon simply has nowhere to render), and the close
// handler hides only when that icon was actually built.
//
// The post-close proof is OUT-OF-WEBVIEW by design: WebKitGTK suspends
// the hidden page and WebKitWebDriver deletes the WebDriver session on
// the first post-close webview command ("session deleted because of
// page crash or hang"), so no webview call can ever pass after the
// close on Linux. The window-hidden half is read from the X server
// instead (xdotool's map-state search — the same source spec 18 uses
// to drive focus on bare Xvfb), and the process-alive half from /proc.
//
// The teardown kills the driver tree (and with it the app) even with
// the window hidden and the session already gone, so nothing leaks
// from this spec.

import { execFile } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";

import { appBinary } from "../harness/app.mjs";

export const name = "keepalive-close";

const pexec = promisify(execFile);
const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;
const APP_ID = "com.acailic.mailchatvault";

/** Window ids of the app's visible (mapped) X11 toplevels, straight
 * from the X server — keeps answering after the close hides the window,
 * unlike the suspended webview. xdotool exits nonzero on no match. */
async function visibleAppWindows() {
  try {
    const { stdout } = await pexec("xdotool", [
      "search",
      "--onlyvisible",
      "--name",
      "Mailchat Vault",
    ]);
    return stdout.trim().split("\n").filter(Boolean);
  } catch {
    return [];
  }
}

/** The main app process for this session: cmdline[0] is exactly the app
 * binary path (WebKit helper processes carry it as a parent argument
 * instead) and the environment carries the session's XDG_DATA_HOME —
 * no other spec's app instance can match both. */
function findMainAppPid(dataDir, binary) {
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const argv0 = readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0")[0];
      if (argv0 !== binary) continue;
      const env = readFileSync(`/proc/${entry}/environ`, "utf8");
      if (env.split("\0").some((v) => v === `XDG_DATA_HOME=${dataDir}`)) {
        return Number(entry);
      }
    } catch {
      // vanished or unreadable — not ours
    }
  }
  return 0;
}

export async function run(ctx) {
  const { withSession, assert, ui, log } = ctx;

  // A settings file exactly like a user's who opted into keep-alive:
  // current version, providers empty, the M7 option explicit on (the
  // autostart toggle stays off — boot must not depend on it).
  const dataDir = await mkdtemp(path.join(tmpdir(), "mailchat-keepalive-"));
  const appDir = path.join(dataDir, APP_ID);
  mkdirSync(appDir, { recursive: true });
  const settingsPath = path.join(appDir, "settings.json");
  await writeFile(
    settingsPath,
    JSON.stringify({
      version: 1,
      providers: [],
      default_provider: null,
      options: {
        theme: "system",
        summarize_top: 8,
        notifications: true,
        keep_running_in_background: true,
        start_at_login: false,
      },
    }),
  );

  await withSession({ dataDir }, async (browser) => {
    await ui.waitForApp(browser);
    await ui.openView(browser, "settings");
    // The settings render settles once summarize-top is filled (the
    // HTML defaults must never race the persisted values).
    await browser.waitUntil(async () => {
      try {
        return (await (await browser.$("#summarize-top")).getValue()) !== "";
      } catch {
        return false;
      }
    }, { timeout: 15_000, timeoutMsg: "settings never rendered (summarize-top stayed empty)" });

    const keepalive = await browser.$("#keepalive-toggle");
    const autostart = await browser.$("#autostart-toggle");
    assert.ok(await keepalive.isSelected(), "the pre-seeded keep-alive option renders checked at boot");
    assert.ok(!(await autostart.isSelected()), "autostart stays off as saved");

    // The window is up and visible — asserted twice: through the webview
    // (pre-close it still answers) and through the X server itself. The
    // X11 read doubles as the tooling sanity check: if xdotool cannot
    // see the mapped window here, its later "no visible window" answer
    // after the close would be worthless, so fail loudly now.
    assert.equal(
      await ui.invokeCommand(browser, "plugin:window|is_visible", { label: "main" }),
      true,
      "the main window is visible before the close",
    );
    await browser.waitUntil(
      async () => (await visibleAppWindows()).length > 0,
      { timeout: 10_000, timeoutMsg: "xdotool never saw the mapped app window before the close (is xdotool installed?)" },
    );

    // Capture the app's own PID for the post-close liveness proof.
    const binary = appBinary();
    const mainPid = findMainAppPid(dataDir, binary);
    assert.ok(mainPid > 0, `found the session's app process (binary ${binary})`);
    log(`      [keepalive-close] app pid ${mainPid}`);

    // The close: driven through the real window API — the exact
    // CloseRequested event the WM close button produces (the WebDriver
    // session's own close command would destroy the webview instead,
    // which is NOT the product's close path). The page suspends right
    // after this, so this is the LAST webview call in the spec.
    await ui.invokeCommand(browser, "plugin:window|close", { label: "main" });

    // Hidden, not gone — proven out of webview: the X server reports no
    // visible app window while the process stays alive under /proc.
    const deadline = Date.now() + 20_000;
    let hidden = false;
    while (Date.now() < deadline) {
      if ((await visibleAppWindows()).length === 0) {
        hidden = true;
        break;
      }
      if (!existsSync(`/proc/${mainPid}`)) {
        // Distinguish a real failure (close exited the app) from a
        // timeout: report it as soon as it happens, not as a slow
        // "window never hid" error.
        assert.fail(`the app process (${mainPid}) exited instead of hiding`);
      }
      await new Promise((r) => setTimeout(r, 200));
    }
    assert.ok(hidden, "no visible app window remains on X11 after the close (the window hid)");
    assert.ok(
      existsSync(`/proc/${mainPid}`),
      `the app process (${mainPid}) is still alive with the window hidden`,
    );
    // Settle: the alive-after-hide state holds, it was not a race where
    // the process exits moments later.
    await new Promise((r) => setTimeout(r, 2_000));
    assert.ok(existsSync(`/proc/${mainPid}`), "the app process is still alive two seconds after the hide settled");
    if ((await visibleAppWindows()).length > 0) {
      assert.fail("an app window came back visible after the hide settled");
    }

    // The saved opt-in was not rewritten by the close.
    const saved = JSON.parse(readFileSync(settingsPath, "utf8"));
    assert.equal(saved.options.keep_running_in_background, true, "the keep-alive opt-in survives the close");
  });
}
