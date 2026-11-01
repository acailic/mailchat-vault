#!/usr/bin/env node
// E2E runner: drives the built Tauri app through tauri-driver/WebDriver,
// one fresh app instance per spec. See tests/e2e/README.md for what is
// covered, how to run locally, and the CI wiring.
//
// Usage:
//   node tests/e2e/run.mjs                 # all specs
//   MAILCHAT_E2E_ONLY=llm node ...         # one spec (name substring)
//   MAILCHAT_E2E_KEEP_DATA=1 node ...      # keep per-spec data dirs

import { spawn } from "node:child_process";
import { readdirSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { appBinary, withSession, vaultDbPath } from "./harness/app.mjs";
import { startMockLlm } from "./harness/mock-llm.mjs";
import { sqliteAvailable, sqliteQuery } from "./harness/sqlite.mjs";
import * as assert from "./harness/assert.mjs";
import * as ui from "./harness/ui.mjs";

const e2eRoot = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(e2eRoot, "..", "..");

class SkipSpec extends Error {
  constructor(reason) {
    super(reason);
    this.name = "SkipSpec";
  }
}

const log = (...args) => process.stdout.write(args.join(" ") + "\n");

function runCommand(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      stdio: ["ignore", "inherit", "inherit"],
    });
    child.on("error", reject);
    child.on("exit", (code) =>
      code === 0 ? resolve() : reject(new Error(`${command} ${args.join(" ")} exited ${code}`)),
    );
  });
}

async function ensureAppBuilt() {
  const binary = appBinary();
  if (existsSync(binary)) {
    return binary;
  }
  log(`building the app first (${binary} not found): frontend + cargo`);
  await runCommand("npm", ["run", "build"]);
  // `tauri/custom-protocol` embeds dist/ and disables the devServer URL —
  // without it a debug binary would try to load localhost:5173 instead of
  // its own assets.
  await runCommand("cargo", [
    "build",
    "-p",
    "mailchat-vault",
    "--features",
    "tauri/custom-protocol",
  ]);
  return binary;
}

async function loadSpecs() {
  const specsDir = path.join(e2eRoot, "specs");
  const files = readdirSync(specsDir)
    .filter((f) => f.endsWith(".e2e.mjs"))
    .sort();
  const specs = [];
  for (const file of files) {
    const mod = await import(path.join(specsDir, file));
    if (typeof mod.run !== "function") {
      throw new Error(`${file} does not export run(ctx)`);
    }
    specs.push({ file, name: mod.name ?? file.replace(/\.e2e\.mjs$/, ""), run: mod.run });
  }
  return specs;
}

async function main() {
  const filter = process.env.MAILCHAT_E2E_ONLY;
  const perSpecTimeoutMs = Number(process.env.MAILCHAT_E2E_TIMEOUT_MS ?? 600_000);
  const binary = await ensureAppBuilt();
  const sqlite = (await sqliteAvailable()) ? { query: sqliteQuery } : null;
  if (!sqlite) {
    log("note: sqlite3 CLI not found — database assertions will be skipped");
  }
  const specs = (await loadSpecs()).filter((s) => !filter || s.name.includes(filter));
  log(`mailchat-vault E2E — app: ${binary} — ${specs.length} spec(s)\n`);

  const results = [];
  for (const spec of specs) {
    const started = Date.now();
    const ctx = {
      repoRoot,
      log,
      assert,
      ui,
      withSession,
      vaultDbPath,
      startMockLlm,
      sqlite,
      skip: (reason) => {
        throw new SkipSpec(reason);
      },
    };
    try {
      await Promise.race([
        spec.run(ctx),
        new Promise((_, reject) =>
          setTimeout(
            () => reject(new Error(`spec exceeded ${perSpecTimeoutMs}ms watchdog`)),
            perSpecTimeoutMs,
          ).unref?.(),
        ),
      ]);
      results.push({ spec, outcome: "pass", seconds: (Date.now() - started) / 1000 });
      log(`PASS  ${spec.name}`);
    } catch (error) {
      if (error instanceof SkipSpec) {
        results.push({ spec, outcome: "skip", reason: error.message });
        log(`SKIP  ${spec.name} — ${error.message}`);
      } else {
        results.push({ spec, outcome: "fail", error });
        log(`FAIL  ${spec.name}`);
        for (const line of String(error.message ?? error).split("\n").slice(0, 8)) {
          log(`      ${line}`);
        }
        if (error.driverLog) {
          log(`      tauri-driver tail: ...${error.driverLog.slice(-600).replace(/\n/g, " | ")}`);
        }
      }
    }
  }

  const passed = results.filter((r) => r.outcome === "pass").length;
  const skipped = results.filter((r) => r.outcome === "skip").length;
  const failed = results.filter((r) => r.outcome === "fail").length;
  log(`\n${passed} passed, ${skipped} skipped, ${failed} failed`);
  if (failed > 0) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  log(`fatal: ${error.stack ?? error}`);
  process.exitCode = 1;
});
