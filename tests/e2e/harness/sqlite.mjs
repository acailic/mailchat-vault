// Database verification straight from the session's SQLite file via the
// `sqlite3` CLI — proves imports landed in the archive, not just in the UI.
// WAL mode lets the CLI read while the app is running. Skippable (returns
// null) when the CLI is not installed.

import { execFile } from "node:child_process";
import { access, constants } from "node:fs/promises";
import { existsSync } from "node:fs";

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
