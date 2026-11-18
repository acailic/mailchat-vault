// Session harness: spawns tauri-driver with per-test environment (a fresh
// XDG_DATA_HOME so every spec starts from an empty archive, plus optional
// LLM configuration), creates a WebDriver session against the built app
// binary, and tears everything down afterwards.
//
// The app process inherits tauri-driver's environment, so per-test env is
// passed to the driver spawn — which is why each spec gets its own
// tauri-driver instance on its own port.

import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import net from "node:net";
import { tmpdir, homedir } from "node:os";
import { stopWalGuards } from "./sqlite.mjs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { remote } from "webdriverio";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/** Locate the built app binary (env override > debug > release). */
export function appBinary() {
  const override = process.env.MAILCHAT_E2E_BINARY;
  if (override) {
    return override;
  }
  const debug = path.join(repoRoot, "target", "debug", "mailchat-vault");
  if (existsSync(debug)) {
    return debug;
  }
  const release = path.join(repoRoot, "target", "release", "mailchat-vault");
  if (existsSync(release)) {
    return release;
  }
  return debug; // callers report "missing" against this path
}

/** Locate tauri-driver (env override > PATH > ~/.cargo/bin). */
function tauriDriverBin() {
  const override = process.env.MAILCHAT_TAURI_DRIVER;
  if (override) {
    return override;
  }
  const cargoBin = path.join(homedir(), ".cargo", "bin", "tauri-driver");
  return existsSync(cargoBin) ? cargoBin : "tauri-driver";
}

/** Locate the WebKitWebDriver binary tauri-driver proxies to (Linux). */
function nativeDriverBin() {
  const override = process.env.MAILCHAT_WEBKIT_DRIVER;
  if (override) {
    return override;
  }
  const candidates = [
    "/usr/libexec/webkit2gtk-4.1/WebKitWebDriver",
    "/usr/bin/WebKitWebDriver",
  ];
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return "WebKitWebDriver"; // let tauri-driver search the PATH
}

/** Reserve a free TCP port by asking the kernel and immediately closing. */
function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

function waitForPort(port, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const socket = net.connect(port, "127.0.0.1");
      socket.once("connect", () => {
        socket.destroy();
        resolve();
      });
      socket.once("error", () => {
        socket.destroy();
        if (Date.now() > deadline) {
          reject(new Error(`tauri-driver did not listen on ${port} within ${timeoutMs}ms`));
        } else {
          setTimeout(attempt, 150);
        }
      });
    };
    attempt();
  });
}

function killTree(child) {
  if (child.exitCode !== null) {
    return;
  }
  // The driver was spawned detached (own process group), so a negative pid
  // signal takes down tauri-driver, WebKitWebDriver and the app together —
  // otherwise the native driver and webkit processes orphan and slow down
  // every later spec.
  try {
    process.kill(-child.pid, "SIGTERM");
  } catch {
    child.kill("SIGTERM");
  }
  const forced = setTimeout(() => {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {
      child.kill("SIGKILL");
    }
  }, 3000);
  child.once("exit", () => clearTimeout(forced));
}

/** Errors worth a fresh driver + session attempt (slow cold app starts can
 * make the first newSession still be mid-flight when a client retry lands,
 * which WebKitWebDriver answers with "max active sessions"). */
const RETRYABLE_SESSION_ERROR =
  /session not created|maximum number of active sessions|UND_ERR_HEADERS_TIMEOUT|UND_ERR_SOCKET|ECONNRESET|ECONNREFUSED/i;

/**
 * Run `fn(browser, ctx)` against a fresh app instance.
 *
 * @param {{ env?: Record<string, string>, dataDir?: string }} options extra
 *   environment for the app (LLM variables); XDG_DATA_HOME is a fresh temp
 *   dir unless `dataDir` names an existing one (restart-over-existing-state
 *   scenarios).
 * @param {(browser: import('webdriverio').Browser, ctx: SessionContext) => Promise<void>} fn
 * @returns {Promise<{ dataDir: string, driverLog: string }>} kept for
 *   post-mortems when `keepData` is set.
 */
export async function withSession(options, fn, { keepData = false } = {}) {
  const attempts = options.attempts ?? 2;
  let lastError;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    const fresh = attempt === attempts; // no retry after the last attempt
    try {
      return await withSessionAttempt(options, fn, { keepData, fresh });
    } catch (error) {
      lastError = error;
      const retryable =
        !fresh && RETRYABLE_SESSION_ERROR.test(String(error?.message ?? error));
      if (!retryable) {
        throw error;
      }
      // Slow cold start: rebuild driver + session from scratch.
    }
  }
  throw lastError;
}

async function withSessionAttempt(options, fn, { keepData }) {
  const extraEnv = options.env ?? {};
  const dataDir = options.dataDir ?? (await mkdtemp(path.join(tmpdir(), "mailchat-e2e-")));
  const port = await freePort();
  const nativePort = await freePort();

  const driver = spawn(
    tauriDriverBin(),
    [
      "--port",
      String(port),
      "--native-port",
      String(nativePort),
      "--native-driver",
      nativeDriverBin(),
    ],
    {
      env: {
        ...process.env,
        ...extraEnv,
        XDG_DATA_HOME: dataDir,
        // Without a compositor (Xvfb / CI) WebKit must not use accelerated
        // compositing, or the webview renders nothing.
        WEBKIT_DISABLE_COMPOSITING_MODE: "1",
        // Skip the GTK accessibility bridge: on CI there is no a11y dbus
        // service, and waiting for it stalls webview startup.
        NO_AT_BRIDGE: "1",
      },
      // Own process group so teardown can signal the whole tree.
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  let driverLog = "";
  driver.stdout.on("data", (chunk) => {
    driverLog += chunk;
  });
  driver.stderr.on("data", (chunk) => {
    driverLog += chunk;
  });

  let browser = null;
  const failures = [];
  try {
    await waitForPort(port, 20_000);
    // tauri-driver forwards /session to the WebKitWebDriver it spawned on
    // --native-port; under CI load that bind can lag tauri-driver's own, and
    // an early request surfaces as UND_ERR_SOCKET / "Connection refused".
    await waitForPort(nativePort, 20_000);
    browser = await remote({
      hostname: "127.0.0.1",
      port,
      capabilities: {
        "tauri:options": { application: appBinary() },
      },
      logLevel: "error",
      waitforInterval: 200,
      waitforTimeout: 10_000,
      // Cold app starts on CI runners can take well over the 30s default;
      // a premature client retry then collides with the still-creating
      // session ("max active sessions"), so be patient instead.
      connectionRetryTimeout: 240_000,
      connectionRetryCount: 1,
    });
    await fn(browser, { dataDir });
  } catch (error) {
    failures.push(error);
  } finally {
    if (browser) {
      try {
        await browser.deleteSession();
      } catch {
        // The driver may already be gone; nothing to clean up then.
      }
    }
    killTree(driver);
    // After the app tree is down, drop the WAL guard children (SIGKILL:
    // an unclean close never unlinks -wal/-shm on its own).
    stopWalGuards();
    if (failures.length === 0 && !keepData && !process.env.MAILCHAT_E2E_KEEP_DATA) {
      await rm(dataDir, { recursive: true, force: true }).catch(() => {});
    }
  }
  if (failures.length > 0) {
    const error = failures[0];
    error.driverLog = driverLog.slice(-4000);
    error.dataDir = process.env.MAILCHAT_E2E_KEEP_DATA ? dataDir : undefined;
    throw error;
  }
  return { dataDir: process.env.MAILCHAT_E2E_KEEP_DATA ? dataDir : "(removed)", driverLog };
}

/** Path of the app's SQLite database for a session's XDG data dir. */
export function vaultDbPath(dataDir) {
  return path.join(dataDir, "com.acailic.mailchatvault", "vault.db");
}
