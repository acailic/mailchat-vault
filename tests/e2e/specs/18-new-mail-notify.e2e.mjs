// Path: the new-mail desktop-notification layer (src/notify.ts) driven at
// the live product surface: a folder-watch import stores new messages and
// one native OS notification lands (the DBus Notify call, captured by
// dbus-monitor, rendered by dunst — the real Linux desktop-notification
// path the Tauri notification plugin takes).
//
// The roadmap contract this spec proves, end to end through the real app:
//   - a watch cycle that stores new messages notifies exactly once, with
//     the count and the file in the body, while the window is unfocused;
//   - a cycle while the window is focused stays silent (the user is
//     already reading in the app);
//   - a cycle that stores nothing new (re-settled file, dedup contract)
//     never notifies;
//   - the Settings checkbox turns the whole layer off and on at runtime;
//   - repeated watch-status snapshots of the same Imported event never
//     duplicate a notification.
//
// Requires the wrapper environment (see tests/e2e/README section in the
// run log): a DBus session bus with dunst as the notification daemon and
// a dbus-monitor capturing Notify calls to MAILCHAT_E2E_NOTIFY_LOG.

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, writeFile, mkdtemp, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "new-mail-notify";

const pexec = promisify(execFile);
const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;
const busLog = process.env.MAILCHAT_E2E_NOTIFY_LOG || null;

function requireBusLog(ctx) {
  if (!busLog) {
    // Hard requirement: without the bus capture this spec cannot honestly
    // observe the OS-notification surface; skip rather than fake a pass.
    ctx.skip("MAILCHAT_E2E_NOTIFY_LOG not set — no notification-bus capture to assert on");
  }
}

/** Whether the app window is currently mapped (focused). A webview
 * screenshot against an unmapped toplevel hangs the WebKit driver until
 * its 240s retry timeouts, so while unfocused only the X11 screen grab
 * is attempted (screenshots are bonus evidence either way; the DBus
 * Notify capture is the hard proof). */
let windowMapped = true;

/** Save proof: a full-screen X11 grab (shows the dunst bubble if one is
 * on screen) plus the webview itself when mapped. */
async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return null;
  await mkdir(evidenceDir, { recursive: true });
  try {
    await pexec("scrot", [`${evidenceDir}/${filename}`]);
  } catch {
    // Screen grab is bonus evidence; never a failure cause.
  }
  if (windowMapped) {
    try {
      await browser.saveScreenshot(`${evidenceDir}/${filename.replace(".png", "-webview.png")}`);
    } catch {
      // ignore
    }
  }
  return `${evidenceDir}/${filename}`;
}

/** Every DBus Notify call captured so far, as blocks of text. */
async function notifyBlocks() {
  const text = await readFile(busLog, "utf8").catch(() => "");
  return text
    .split(/(?=method call time=)/)
    .filter((b) => b.includes("member=Notify"));
}

/** The `member=Notify` call count in the capture so far. */
async function notifyCount() {
  return (await notifyBlocks()).length;
}

/** Fail unless exactly `expected` new Notify calls appeared since `before`. */
async function expectNotifications(before, expected, msg) {
  const count = await notifyCount();
  if (count !== before + expected) {
    const blocks = (await notifyBlocks()).map((b) => b.replace(/\s+/g, " ").slice(0, 220));
    throw new Error(
      `${msg}: expected ${before} + ${expected} Notify calls, saw ${count}. ` +
        `Calls so far:\n${blocks.join("\n") || "(none)"}`,
    );
  }
}

/** The notification title/body strings captured so far (arg_utf8 strings). */
async function notifyTexts() {
  return (await notifyBlocks()).map((b) => b.replace(/\s+/g, " "));
}

// --- X11 focus control (bare Xvfb, no window manager) -----------------

let rootWin = null;
let appWin = null;

async function ensureWindows(browser) {
  if (rootWin && appWin) return;
  const { stdout } = await pexec("xwininfo", ["-root"]);
  rootWin = stdout.match(/Window id: (0x[0-9a-f]+)/)?.[1];
  const found = await pexec("xdotool", ["search", "--onlyvisible", "--name", "Mailchat Vault"]);
  appWin = found.stdout.trim().split("\n").pop();
  if (!rootWin || !appWin) {
    throw new Error(`could not find X windows: root=${rootWin} app=${appWin}`);
  }
}

/** Move X input focus between the app window and the root window, and
 * wait until the webview itself agrees (document.hasFocus). */
async function setFocused(browser, focused, label) {
  await ensureWindows(browser);
  // Focus control on bare Xvfb (no window manager): plain `windowfocus`
  // on the root window only takes the first time — WebKit latches the
  // active state and later root-focus moves never reach the webview.
  // Core-protocol unmap/map does: unmapping the toplevel always
  // unfocuses it, and mapping + focusing restores. (Proven by the
  // debug-focus3 probe: three full cycles, no WM needed.)
  if (focused) {
    await pexec("xdotool", ["windowmap", appWin]);
    await pexec("xdotool", ["windowfocus", appWin]);
  } else {
    await pexec("xdotool", ["windowunmap", appWin]);
  }
  windowMapped = focused;
  let last = null;
  await browser.waitUntil(
    async () => {
      last = await browser.execute(() => document.hasFocus());
      return last === focused;
    },
    {
      timeout: 10_000,
      timeoutMsg: `document.hasFocus() never became ${focused} after xdotool windowfocus (${label}); last=${last}`,
    },
  );
}

function mail(id, subject) {
  return [
    `From alice@example.com Sat Nov  1 10:0${id}0:00 2026`,
    `From: Alice Smith <alice@example.com>`,
    `To: Bob <bob@example.com>`,
    `Subject: ${subject}`,
    `Date: Sat, 1 Nov 2026 10:0${id}:00 +0100`,
    `Message-ID: <${id}@watch.local>`,
    ``,
    `body of ${subject}`,
    ``,
  ].join("\n");
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;
  requireBusLog(ctx);

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    await ui.openSourcesSection(browser, "automatic");

    const watched = await mkdtemp(path.join(tmpdir(), "mailchat-watch-"));
    await writeFile(path.join(watched, "first.mbox"), mail(1, "One") + mail(2, "Two"));
    let before = await notifyCount();

    // ==============================================================
    // 1. Unfocused + new messages: exactly one notification, correct
    //    title/body, rendered by the desktop notification daemon.
    // ==============================================================
    await setFocused(browser, false, "before first import");
    await (await browser.$("#watch-path")).setValue(watched);
    await (await browser.$("#watch-btn")).click();
    await ui.waitForText(browser, "#stats", "2 messages (mbox: 2)", {
      timeout: 30_000,
      timeoutMsg: "initial scan never imported the mbox",
    });
    await saveScreenshot(browser, "18-notify-import-unfocused.png");
    await expectNotifications(before, 1, "first watch import while unfocused");
    const texts = await notifyTexts();
    assert.ok(
      texts.some((t) => t.includes("2 new messages")),
      `notification title counts the new mail, got: ${texts.join(" | ")}`,
    );
    assert.ok(
      texts.some((t) => t.includes("imported from first.mbox")),
      `notification body names the watched file, got: ${texts.join(" | ")}`,
    );

    // ==============================================================
    // 2. Focused + new messages: silent — the user is already reading.
    // ==============================================================
    await setFocused(browser, true, "focus gate");
    before = await notifyCount();
    await writeFile(path.join(watched, "second.mbox"), mail(3, "Three"));
    await ui.waitForText(browser, "#stats", "3 messages (mbox: 3)", {
      timeout: 30_000,
      timeoutMsg: "focused-window import never stored the message",
    });
    await expectNotifications(before, 0, "import while the window is focused");

    // ==============================================================
    // 3. Nothing new: re-settling an already-imported file (dedup
    //    contract) stores nothing, so nothing notifies.
    // ==============================================================
    before = await notifyCount();
    await writeFile(path.join(watched, "second.mbox"), mail(3, "Three"));
    await sleep(4000);
    assert.equal(
      await (await browser.$("#stats")).getText(),
      "3 messages (mbox: 3)",
      "re-settled file stayed deduped",
    );
    await expectNotifications(before, 0, "re-settled file with nothing new");

    // ==============================================================
    // 4. Settings off: even a cycle with new mail stays silent.
    // ==============================================================
    await ui.openSettingsSection(browser, "general");
    const toggle = await browser.$("#notify-toggle");
    assert.ok(await toggle.isSelected(), "Desktop notifications on by default");
    await toggle.click();
    await (await browser.$("#settings-save-general")).click();
    await ui.waitForText(
      browser,
      "#settings-note",
      (text) => text.startsWith("Saved."),
      { timeoutMsg: "settings save never confirmed" },
    );

    await setFocused(browser, false, "settings off");
    before = await notifyCount();
    await writeFile(path.join(watched, "second.mbox"), mail(3, "Three") + mail(4, "Four"));
    await ui.waitForText(browser, "#stats", "4 messages (mbox: 4)", {
      timeout: 30_000,
      timeoutMsg: "opted-out cycle never stored the message",
    });
    await expectNotifications(before, 0, "cycle with new mail while the option is off");

    // ==============================================================
    // 5. Settings back on (no restart): notifications return, one per
    //    cycle, still no duplicates from repeated watch snapshots.
    // ==============================================================
    await ui.openSettingsSection(browser, "general");
    await (await browser.$("#notify-toggle")).click();
    await (await browser.$("#settings-save-general")).click();
    await ui.waitForText(
      browser,
      "#settings-note",
      (text) => text.startsWith("Saved."),
      { timeoutMsg: "settings re-enable never confirmed" },
    );

    await setFocused(browser, false, "settings back on");
    before = await notifyCount();
    await writeFile(path.join(watched, "second.mbox"), mail(3, "Three") + mail(4, "Four") + mail(5, "Five"));
    await ui.waitForText(browser, "#stats", "5 messages (mbox: 5)", {
      timeout: 30_000,
      timeoutMsg: "re-enabled cycle never stored the message",
    });
    await saveScreenshot(browser, "18-notify-re-enabled.png");
    await expectNotifications(before, 1, "re-enabled cycle notifies once");
    const texts2 = await notifyTexts();
    assert.ok(
      texts2.some((t) => t.includes("1 new message") && t.includes("imported from second.mbox")),
      `re-enabled notification counts and names the file, got: ${texts2.join(" | ")}`,
    );

    // Dedup: hold the window unfocused across several watch poll cycles
    // (the same Imported event stays newest) — still exactly one.
    await sleep(5000);
    await expectNotifications(before, 1, "repeated watch snapshots never duplicate");
  });
}
