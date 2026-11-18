// Database verification straight from the session's SQLite file via the
// `sqlite3` CLI — proves imports landed in the archive, not just in the UI.
// WAL mode lets the CLI read while the app is running. Skippable (returns
// null) when the CLI is not installed.
//
// WAL guard: a one-shot `sqlite3` CLI process that CLEANLY closes while
// the app's pool is idle grabs the exclusive lock, checkpoints, and
// DELETES vault.db-wal/-shm (SQLite's "last connection" shutdown). The
// app's pool connections keep writing into the unlinked inode — every
// commit after that is invisible to new connections and dies with the
// process (the #271 split). The guard is a python child holding an open
// read transaction on the same file, so the CLI's close never sees
// itself as the last connection and never unlinks the WAL.

import { execFile, spawn } from "node:child_process";
import { access, constants } from "node:fs/promises";
import { existsSync } from "node:fs";

const walGuards = new Map();

/** Hold a read transaction on `dbPath` so sqlite3 CLI closes cannot
 *  unlink the live -wal/-shm (#271). Best-effort: no python3, no guard
 *  (specs that never CLI-write mid-session never needed one). Returns a
 *  promise that resolves once the guard holds its transaction (or after
 *  a 2s fallback — a late guard is still better than none). */
export function startWalGuard(dbPath) {
  const existing = walGuards.get(dbPath);
  if (existing) return existing.ready;
  let resolveReady;
  const ready = new Promise((resolve) => {
    resolveReady = resolve;
    setTimeout(resolve, 2000);
  });
  try {
    const child = spawn(
      "python3",
      [
        "-c",
        [
          "import sqlite3, sys, time",
          "db = sys.argv[1]",
          "c = sqlite3.connect(db, timeout=5, isolation_level=None)",
          "c.execute('BEGIN')",
          "c.execute('SELECT count(*) FROM sqlite_master').fetchone()",
          "print('READY', flush=True)",
          "while True: time.sleep(60)",
        ].join("\n"),
        dbPath,
      ],
      { stdio: ["ignore", "pipe", "ignore"] },
    );
    child.stdout.once("data", () => resolveReady());
    child.on("error", () => {
      walGuards.delete(dbPath);
      resolveReady();
    });
    walGuards.set(dbPath, { child, ready });
  } catch {
    // No guard is better than a broken harness.
  }
  return ready;
}

/** Kill every WAL guard (SIGKILL: an unclean exit never unlinks). */
export function stopWalGuards() {
  for (const { child } of walGuards.values()) {
    try {
      child.kill("SIGKILL");
    } catch {}
  }
  walGuards.clear();
}

export async function sqliteAvailable() {
  return new Promise((resolve) => {
    execFile("sqlite3", ["-version"], (error) => resolve(!error));
  });
}

/**
 * Run one SQL statement against a database, returning rows as objects
 * (`sqlite3 -json`). Resolves to null when the CLI or file is unavailable.
 */
export async function sqliteQuery(dbPath, sql) {
  if (!existsSync(dbPath)) {
    return null;
  }
  await startWalGuard(dbPath);
  await access(dbPath, constants.R_OK).catch(() => null);
  return new Promise((resolve, reject) => {
    execFile("sqlite3", ["-json", dbPath, sql], { maxBuffer: 10 * 1024 * 1024 }, (error, stdout) => {
      if (error) {
        reject(new Error(`sqlite3 failed for ${dbPath}: ${error.message}`));
        return;
      }
      if (!stdout.trim()) {
        resolve([]);
        return;
      }
      try {
        resolve(JSON.parse(stdout));
      } catch (parseError) {
        reject(new Error(`sqlite3 returned non-JSON output: ${stdout.slice(0, 200)}`));
      }
    });
  });
}
