// @vitest-environment node
// Self-checks for the native-verification substrate (G1 / A14 / #81).
//
// The macOS and Windows release smokes cannot run on the dev/CI Linux
// machines (they need real native hosts — that is the point of the
// gate), so this suite guards the parts any machine can verify:
//
//   - the scripts parse (bash -n; PowerShell parser when pwsh exists)
//   - shellcheck is clean at warning level when shellcheck is installed
//   - each smoke refuses to run off-platform with a clear error
//   - the G1 evidence contract is present in the scripts: artifact
//     SHA-256 binding, tester, UTC timestamps, machine-readable JSON
//     output, and a greppable RESULT line
//   - the evidence machinery itself works: smoke-macos.sh is exercised
//     end-to-end under stubbed macOS tooling and must emit valid JSON
//     (PASS and FAIL paths) with digests and tester bound
//   - the runbook names the exact command per OS and the A14 templates
//     carry the fields the gate requires
//
// These checks never substitute for a native run
// (docs/native-verification.md).
import { describe, expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const smokeMacos = `${repoRoot}scripts/release/smoke-macos.sh`;
const dmgSmoke = `${repoRoot}scripts/dmg-smoke-test.sh`;
const windowsSmoke = `${repoRoot}scripts/windows-smoke.ps1`;
const windowsFromRelease = `${repoRoot}scripts/release/windows-smoke-from-release.ps1`;
const runbook = `${repoRoot}docs/native-verification.md`;

function toolAvailable(name: string): boolean {
  const r = spawnSync("bash", ["-c", `command -v ${name}`], { stdio: "ignore" });
  return r.status === 0;
}

const hasBash = toolAvailable("bash");
const hasShellcheck = toolAvailable("shellcheck");
const hasPwsh = toolAvailable("pwsh");

describe("release smoke scripts parse", () => {
  it.skipIf(!hasBash)("bash -n accepts the macOS smoke scripts", () => {
    for (const script of [smokeMacos, dmgSmoke]) {
      execFileSync("bash", ["-n", script]);
    }
  });

  it.skipIf(!hasShellcheck)("shellcheck reports no warnings for smoke-macos.sh", () => {
    execFileSync("shellcheck", ["--severity=warning", smokeMacos]);
  });

  it.skipIf(!hasPwsh)("PowerShell parser accepts the Windows smoke scripts", () => {
    const checkerScript = [
      "param([Parameter(ValueFromRemainingArguments=$true)][string[]]$Files)",
      "$fail = 0",
      "foreach ($f in $Files) {",
      "    $tokens = $null; $errors = $null",
      "    [System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$tokens, [ref]$errors) | Out-Null",
      "    if ($errors.Count -gt 0) { $fail = 1; Write-Output (\"PARSE-ERRORS in {0}: {1}\" -f $f, ($errors | ForEach-Object Message)) }",
      "}",
      "exit $fail",
    ].join("\n");
    const checker = `${tmpdir()}/mc-parse-check-${process.pid}.ps1`;
    writeFileSync(checker, checkerScript);
    const r = spawnSync("pwsh", ["-NoProfile", "-File", checker, windowsSmoke, windowsFromRelease], {
      encoding: "utf8",
    });
    expect(r.status).toBe(0);
  });
});

describe("off-platform refusal", () => {
  it.skipIf(!hasBash || process.platform !== "linux")(
    "smoke-macos.sh refuses to run on Linux with a clear error",
    () => {
      const r = spawnSync("bash", [smokeMacos, "--dir", "."], { encoding: "utf8" });
      expect(r.status).toBe(2);
      expect(r.stderr).toContain("must run on macOS");
    },
  );

  it.skipIf(!hasPwsh || process.platform !== "linux")(
    "windows-smoke.ps1 refuses to run without a Windows profile",
    () => {
      const r = spawnSync(
        "pwsh",
        ["-NoProfile", "-File", windowsSmoke, "-NewMsi", "x.msi", "-NewExe", "y.exe"],
        { encoding: "utf8", env: { ...process.env, APPDATA: "", ProgramFiles: "" } },
      );
      expect(r.status).not.toBe(0);
      expect(r.stderr).toContain("must run on Windows");
    },
  );
});

describe("G1 evidence contract in the smoke scripts", () => {
  const macos = readFileSync(smokeMacos, "utf8");
  const win = readFileSync(windowsSmoke, "utf8");
  const wrapper = readFileSync(windowsFromRelease, "utf8");

  it("smoke-macos.sh binds artifacts by SHA-256, tester, and timestamps, and writes JSON", () => {
    expect(macos).toContain("shasum -a 256");
    expect(macos).toContain("--tester");
    expect(macos).toContain("MC_SMOKE_TESTER");
    expect(macos).toContain("mailchat-vault/smoke-macos/1");
    expect(macos).toContain("started_utc");
    expect(macos).toContain("finished_utc");
    expect(macos).toContain('"tester"');
    expect(macos).toContain("RESULT: PASS");
    expect(macos).toContain("RESULT: FAIL");
  });

  it("windows-smoke.ps1 binds installers by SHA-256, tester, and timestamps, and writes summary.json", () => {
    expect(win).toContain("Get-FileHash -Algorithm SHA256");
    expect(win).toContain("mailchat-vault/windows-smoke/1");
    expect(win).toContain("summary.json");
    expect(win).toContain("started_utc");
    expect(win).toContain("finished_utc");
    expect(win).toContain("Tester");
    expect(win).toContain("RESULT: PASS");
    expect(win).toContain("RESULT: FAIL");
  });

  it("windows-smoke-from-release.ps1 passes the tester through and prints downloaded digests", () => {
    expect(wrapper).toContain("Get-FileHash -Algorithm SHA256");
    expect(wrapper).toContain("Tester = $Tester");
  });
});

describe("smoke-macos.sh evidence machinery (stubbed macOS tooling)", () => {
  it.skipIf(!hasBash || process.platform !== "linux")(
    "emits valid PASS/FAIL JSON bound to artifact SHA-256 and tester",
    async () => {
      const work = `${tmpdir()}/mc-smoke-selfcheck-${process.pid}`;
      rmSync(work, { recursive: true, force: true });
      const bin = `${work}/bin`;
      const art = `${work}/artifacts`;
      mkdirSync(bin, { recursive: true });
      mkdirSync(art, { recursive: true });

      // Fake .app inside a fake dmg (any file plays the dmg; the mocked
      // hdiutil populates the mountpoint) + a real updater tarball.
      const appName = "Mailchat Vault.app";
      mkdirSync(`${art}/unpack/${appName}/Contents/MacOS`, { recursive: true });
      writeFileSync(
        `${art}/unpack/${appName}/Contents/Info.plist`,
        "<plist><dict><key>CFBundleIdentifier</key><string>com.acailic.mailchatvault</string>" +
          "<key>CFBundleShortVersionString</key><string>0.1.0</string>" +
          "<key>CFBundleExecutable</key><string>mailchat-vault</string></dict></plist>",
      );
      // The fake app only needs to outlive the smoke script's alive
      // check — that is observable state, not a duration: it parks
      // indefinitely (an empty interval keeps the event loop alive),
      // exits cleanly on the SIGTERM the script's alive loop sends, and
      // writes a ready file (its stdout is redirected to /dev/null by
      // the script, so a file is the observable signal) so the test can
      // assert the app process really started. The ready-file path is
      // baked in before launch for the same reason.
      const appStarted = `${work}/app-started`;
      writeFileSync(
        `${art}/unpack/${appName}/Contents/MacOS/mailchat-vault`,
        "#!/usr/bin/env node\n" +
          `require("node:fs").writeFileSync(${JSON.stringify(appStarted)}, "ready");\n` +
          'process.on("SIGTERM", () => process.exit(0));\n' +
          "setInterval(() => {}, 1 << 30);\n",
        { mode: 0o755 },
      );
      const dmg = `${art}/mailchat-vault_0.1.0_universal.dmg`;
      writeFileSync(dmg, "fake dmg bytes");
      execFileSync("tar", [
        "-czf",
        `${art}/mailchat-vault_0.1.0_universal.app.tar.gz`,
        "-C",
        `${art}/unpack`,
        appName,
      ]);

      const shims: Record<string, string> = {
        uname: 'case "$1" in -m) echo arm64;; *) echo Darwin;; esac',
        sw_vers: 'case "$1" in -productName) echo "macOS";; *) echo 15.1;; esac',
        hdiutil: `if [[ "$1" == attach ]]; then mp=""; prev=""; for a in "$@"; do [[ "$prev" == -mountpoint ]] && mp="$a"; prev="$a"; done; mkdir -p "$mp"; cp -R '${art}/unpack/.' "$mp/"; exit 0; elif [[ "$1" == detach ]]; then [[ -d "$2" ]] && rm -rf "$2"; exit 0; fi; exit 1`,
        plutil:
          'case "$2" in CFBundleIdentifier) echo com.acailic.mailchatvault;; CFBundleShortVersionString) echo 0.1.0;; CFBundleExecutable) echo mailchat-vault;; *) exit 1;; esac',
        codesign: 'echo "Signature=adhoc" >&2',
        lipo: 'echo "x86_64 arm64"',
        xattr: "exit 0",
      };
      for (const [name, body] of Object.entries(shims)) {
        writeFileSync(`${bin}/${name}`, `#!/usr/bin/env bash\n${body}\n`, { mode: 0o755 });
      }
      // shasum shim that runs wherever node does
      writeFileSync(
        `${bin}/shasum`,
        '#!/usr/bin/env node\n' +
          'const c = require("node:crypto"), f = require("node:fs");\n' +
          'const p = process.argv[process.argv.length - 1];\n' +
          'console.log(c.createHash("sha256").update(f.readFileSync(p)).digest("hex") + "  " + p);\n',
        { mode: 0o755 },
      );

      const runSmoke = (logsDir: string) =>
        spawnSync(
          "bash",
          [
            smokeMacos,
            "--dir",
            art,
            "--skip-updater",
            "--tester",
            "Selfcheck Tester",
            "--alive",
            "2",
            "--logs",
            logsDir,
          ],
          { encoding: "utf8", env: { ...process.env, PATH: `${bin}:${process.env.PATH}` } },
        );

      // Bounded condition polls for everything that appears on disk
      // (50 ms interval; the timeout is only a safety bound, never a
      // wait target). No fake timers: real child processes below.
      const waitForOpts = { timeout: 10_000, interval: 50 };

      // PASS run: every check green, valid JSON bound to real digests.
      const passLogs = `${work}/logs-pass`;
      const pass = runSmoke(passLogs);
      expect(pass.status).toBe(0);
      // Observable state, not elapsed time: the stub app process really
      // started, and exactly one evidence JSON landed.
      await vi.waitFor(() => expect(existsSync(appStarted)).toBe(true), waitForOpts);
      let passJsonFiles: string[] = [];
      await vi.waitFor(() => {
        passJsonFiles = readdirSync(passLogs).filter((f) => f.endsWith(".json"));
        expect(passJsonFiles).toHaveLength(1);
      }, waitForOpts);
      const evidence = JSON.parse(readFileSync(`${passLogs}/${passJsonFiles[0]}`, "utf8"));
      expect(evidence.schema).toBe("mailchat-vault/smoke-macos/1");
      expect(evidence.result).toBe("PASS");
      expect(evidence.tester).toBe("Selfcheck Tester");
      expect(evidence.started_utc).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
      expect(evidence.finished_utc).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/);
      expect(evidence.host).toEqual({ os: "macOS", os_version: "15.1", arch: "arm64" });
      expect(evidence.repo.commit).toMatch(/^[0-9a-f]{40}$/);
      const expectedSha = createHash("sha256").update(readFileSync(dmg)).digest("hex");
      expect(evidence.artifacts[0]).toEqual({
        name: "mailchat-vault_0.1.0_universal.dmg",
        sha256: expectedSha,
      });
      expect(evidence.checks.length).toBeGreaterThan(0);
      expect(evidence.checks.every((c: { status: string }) => c.status === "PASS")).toBe(true);
      expect(evidence.failed).toBe(0);

      // FAIL run: broken bundle -> exit 1, still-valid JSON with result FAIL.
      rmSync(`${art}/unpack/${appName}/Contents/Info.plist`);
      const failLogs = `${work}/logs-fail`;
      const fail = runSmoke(failLogs);
      expect(fail.status).toBe(1);
      let failJsonFiles: string[] = [];
      await vi.waitFor(() => {
        failJsonFiles = readdirSync(failLogs).filter((f) => f.endsWith(".json"));
        expect(failJsonFiles).toHaveLength(1);
      }, waitForOpts);
      const failEvidence = JSON.parse(readFileSync(`${failLogs}/${failJsonFiles[0]}`, "utf8"));
      expect(failEvidence.result).toBe("FAIL");
      expect(failEvidence.failed).toBeGreaterThan(0);

      rmSync(work, { recursive: true, force: true });
    },
    120_000,
  );
});

describe("runbook and A14 evidence templates", () => {
  it("runbook exists and names the exact native commands per OS", () => {
    const md = readFileSync(runbook, "utf8");
    expect(md).toContain("smoke-macos.sh --release");
    expect(md).toContain("smoke-macos.sh --dir");
    expect(md).toContain('--tester "Your Name"');
    expect(md).toContain("windows-smoke-from-release.ps1 -Tag");
    expect(md).toContain('-Tester "Your Name"');
    expect(md).toContain("#81");
    expect(md).toContain("evidence/native-verification-templates");
  });

  const templates: Array<[string, string]> = [
    ["macOS", `${repoRoot}docs/evidence/native-verification-templates/macos.md`],
    ["Windows", `${repoRoot}docs/evidence/native-verification-templates/windows.md`],
  ];

  for (const [os, file] of templates) {
    it(`${os} template binds evidence and covers the G1 journey`, () => {
      expect(existsSync(file)).toBe(true);
      const md = readFileSync(file, "utf8");
      // G1 binding fields
      expect(md).toContain("SHA-256");
      expect(md).toContain("Tester");
      expect(md).toContain("Date (UTC)");
      // journey steps
      for (const step of ["Install", "Import", "Find", "Export", "Backup", "Restore", "Upgrade", "Uninstall"]) {
        expect(md).toContain(step);
      }
      // honesty rule
      expect(md).toContain("not run");
    });
  }

  it("template index links the runbook and the support-matrix loop", () => {
    const md = readFileSync(
      `${repoRoot}docs/evidence/native-verification-templates/README.md`,
      "utf8",
    );
    expect(md).toContain("native-verification.md");
    expect(md).toContain("#81");
  });
});
