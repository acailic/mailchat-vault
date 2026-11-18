// @vitest-environment node
// Self-checks for scripts/install.sh (the one-command installer).
//
// Real installs need clean Ubuntu/Fedora/macOS hosts that this runner
// cannot provide (no Docker, no Tauri, no WebKitGTK, no /Applications),
// so — like src/release-smoke.test.ts — this suite guards what any
// machine can verify:
//
//   - the script parses as POSIX sh (sh -n, dash here) and bash
//   - shellcheck is clean at warning level when shellcheck is installed
//   - stubbed end-to-end runs (PATH shims for curl/wget, uname, sudo,
//     apt-get, dpkg-query, xattr, open, ...) covering:
//       asset selection per platform (deb, AppImage, macOS .app.tar.gz),
//       the pinned-version path (--version / MAILCHAT_VERSION),
//       draft refusal,
//       the checksum-mismatch abort (non-zero exit, nothing installed,
//       temp files cleaned),
//       the missing-SHA256SUMS-entry abort,
//       idempotent re-runs / upgrade-in-place notes,
//       --uninstall keeping the vault data dir,
//       and the wget fallback when curl is absent.
//   - the docs contract: "Install in one line" in the README Quickstart
//     and docs/download.md, the read-it-first note, and no Windows
//     one-liner advertised (install.ps1 is a separate later part).
//
// These checks never substitute for native install runs
// (docs/native-verification.md).
import { describe, expect, it, vi } from "vitest";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const installSh = `${repoRoot}scripts/install.sh`;
const readmeMd = readFileSync(`${repoRoot}README.md`, "utf8");
const downloadMd = readFileSync(`${repoRoot}docs/download.md`, "utf8");

const ONE_LINER =
  "curl -fsSL https://raw.githubusercontent.com/acailic/mailchat-vault/main/scripts/install.sh | sh";

function toolAvailable(name: string): boolean {
  const r = spawnSync("bash", ["-c", `command -v ${name}`], { stdio: "ignore" });
  return r.status === 0;
}

const hasSh = toolAvailable("sh");
const hasBash = toolAvailable("bash");
const hasShellcheck = toolAvailable("shellcheck");
const hasSetsid = toolAvailable("setsid");

// -------------------------------------------------------------- stub world
//
// A sandbox with PATH shims that stand in for every external tool the
// installer uses: the GitHub API and asset downloads are served from
// local fixtures via a URL->file map, and side-effecting commands
// (sudo, apt-get, xattr, open, ...) only log. HOME and TMPDIR point
// into the sandbox, so installs land in the sandbox and the temp-dir
// cleanup is observable.

type Kind = "deb" | "appimage" | "macos";

interface WorldOpts {
  kind: Kind;
  pinVersion?: string; // serve releases/tags/v<pinVersion>.json
  draft?: boolean; // the pinned fixture is a draft
  tamper?: "appimage" | "deb" | "mac"; // corrupt after SHA256SUMS is written
  omitSumsEntry?: "appimage" | "deb"; // SHA256SUMS lacks this asset's line
  haveFuse?: boolean;
  noCurl?: boolean; // PATH without curl: the wget fallback runs
  debInstalled?: boolean; // dpkg-query shim reports the package installed
  preinstall?: boolean; // seed an existing install (upgrade notes)
}

interface World {
  work: string;
  home: string;
  tmp: string;
  vaultDb: string;
  calls: string; // every URL fetched
  sudoLog: string;
  xattrLog: string;
  openLog: string;
  env: NodeJS.ProcessEnv;
  run(args?: string[], extraEnv?: Record<string, string>): ReturnType<typeof spawnSync>;
  isolatedRun(args: string[], extraEnv?: Record<string, string>): ReturnType<typeof spawnSync>;
}

const LATEST_VERSION = "0.2.0";

function writeShim(dir: string, name: string, body: string): void {
  writeFileSync(join(dir, name), `#!/bin/sh\n${body}\n`, { mode: 0o755 });
}

function sha256OfFile(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function buildWorld(opts: WorldOpts): World {
  const work = mkdtempSync(join(tmpdir(), "mc-install-"));
  const home = join(work, "home");
  const tmp = join(work, "tmp");
  const bin = join(work, "bin");
  const fixtures = join(work, "fixtures");
  for (const d of [home, tmp, bin, fixtures]) mkdirSync(d);

  // The vault data dir with a database that must survive every path.
  const dataRel =
    opts.kind === "macos"
      ? "Library/Application Support/com.acailic.mailchatvault"
      : ".local/share/com.acailic.mailchatvault";
  const vaultDb = join(home, dataRel, "vault.db");
  mkdirSync(join(home, dataRel), { recursive: true });
  writeFileSync(vaultDb, "vault db fixture - must survive");

  // Fake release assets. The AppImage "binary" is a script that fakes
  // --appimage-extract (producing the bundled icon) and records a launch.
  const launchMark = join(work, "launch-mark");
  const makeAssetSet = (version: string) => {
    const deb = join(fixtures, `mailchat-vault_${version}_amd64.deb`);
    writeFileSync(deb, `fake deb payload ${version}\n`);
    const appimage = join(fixtures, `mailchat-vault_${version}_amd64.AppImage`);
    writeFileSync(
      appimage,
      [
        "#!/bin/sh",
        'case "${1:-}" in',
        "  --appimage-extract)",
        "    mkdir -p squashfs-root",
        "    printf 'fake icon png' > squashfs-root/mailchat-vault.png",
        "    exit 0",
        "    ;;",
        "esac",
        'if [ -n "${MC_LAUNCH_MARK:-}" ]; then touch "$MC_LAUNCH_MARK"; fi',
        "exit 0",
        "",
      ].join("\n"),
      { mode: 0o755 },
    );
    return { deb, appimage };
  };
  const latest = makeAssetSet(LATEST_VERSION);

  // The macOS asset: a real tar.gz holding a fake mailchat-vault.app
  // (the real asset has no version in its name).
  const macStaging = join(work, "macstage");
  mkdirSync(join(macStaging, "mailchat-vault.app/Contents/MacOS"), { recursive: true });
  writeFileSync(
    join(macStaging, "mailchat-vault.app/Contents/MacOS/mailchat-vault"),
    "#!/bin/sh\nexit 0\n",
    { mode: 0o755 },
  );
  writeFileSync(
    join(macStaging, "mailchat-vault.app/Contents/Info.plist"),
    `<?xml version="1.0"?><plist><dict><key>CFBundleShortVersionString</key><string>${LATEST_VERSION}</string></dict></plist>`,
  );
  const macTgz = join(fixtures, "mailchat-vault.app.tar.gz");
  execFileSync("tar", ["-czf", macTgz, "-C", macStaging, "mailchat-vault.app"]);

  const releaseJsonPath = (version: string, draft: boolean): string => {
    const names = [
      "mailchat-vault.app.tar.gz",
      `mailchat-vault_${version}_amd64.AppImage`,
      `mailchat-vault_${version}_amd64.deb`,
      `mailchat-vault_${version}_universal.dmg`,
      `mailchat-vault_${version}_x64_en-US.msi`,
      `mailchat-vault_${version}_x64-setup.exe`,
      "SHA256SUMS",
    ];
    const p = join(fixtures, `release-${version}${draft ? "-draft" : ""}.json`);
    writeFileSync(
      p,
      JSON.stringify(
        {
          tag_name: `v${version}`,
          draft,
          prerelease: false,
          assets: names.map((name) => ({
            name,
            browser_download_url: `https://github.com/acailic/mailchat-vault/releases/download/v${version}/${name}`,
          })),
        },
        null,
        2,
      ),
    );
    return p;
  };
  const sumsPath = (version: string): string => {
    const p = join(fixtures, `SHA256SUMS-${version}`);
    const lines = [
      `${sha256OfFile(macTgz)}  mailchat-vault.app.tar.gz`,
      `${sha256OfFile(join(fixtures, `mailchat-vault_${version}_amd64.AppImage`))}  mailchat-vault_${version}_amd64.AppImage`,
      `${sha256OfFile(join(fixtures, `mailchat-vault_${version}_amd64.deb`))}  mailchat-vault_${version}_amd64.deb`,
    ].filter((line) => !(opts.omitSumsEntry === "appimage" && line.endsWith("_amd64.AppImage")));
    writeFileSync(p, `${lines.join("\n")}\n`);
    return p;
  };

  const dl = (version: string, name: string) =>
    `https://github.com/acailic/mailchat-vault/releases/download/v${version}/${name}`;
  const urlmap = join(work, "urlmap");
  const mapEntries: Array<[string, string]> = [
    [
      "https://api.github.com/repos/acailic/mailchat-vault/releases/latest",
      releaseJsonPath(LATEST_VERSION, false),
    ],
    [dl(LATEST_VERSION, "SHA256SUMS"), sumsPath(LATEST_VERSION)],
    [dl(LATEST_VERSION, "mailchat-vault.app.tar.gz"), macTgz],
    [dl(LATEST_VERSION, `mailchat-vault_${LATEST_VERSION}_amd64.deb`), latest.deb],
    [dl(LATEST_VERSION, `mailchat-vault_${LATEST_VERSION}_amd64.AppImage`), latest.appimage],
  ];
  if (opts.pinVersion) {
    const pinned = makeAssetSet(opts.pinVersion);
    mapEntries.push(
      [
        `https://api.github.com/repos/acailic/mailchat-vault/releases/tags/v${opts.pinVersion}`,
        releaseJsonPath(opts.pinVersion, opts.draft === true),
      ],
      [dl(opts.pinVersion, "SHA256SUMS"), sumsPath(opts.pinVersion)],
      [dl(opts.pinVersion, "mailchat-vault.app.tar.gz"), macTgz],
      [dl(opts.pinVersion, `mailchat-vault_${opts.pinVersion}_amd64.deb`), pinned.deb],
      [dl(opts.pinVersion, `mailchat-vault_${opts.pinVersion}_amd64.AppImage`), pinned.appimage],
    );
  }
  writeFileSync(urlmap, mapEntries.map(([u, p]) => `${u}\t${p}`).join("\n") + "\n");

  // Tamper AFTER the sums were computed: the served bytes no longer match.
  if (opts.tamper === "appimage") {
    writeFileSync(latest.appimage, `tampered appimage bytes ${LATEST_VERSION}\n`, { mode: 0o755 });
  } else if (opts.tamper === "deb") {
    writeFileSync(latest.deb, "tampered deb bytes\n");
  } else if (opts.tamper === "mac") {
    writeFileSync(macTgz, "tampered mac tarball bytes\n");
  }

  const calls = join(work, "fetch-calls");
  writeFileSync(calls, "");
  const fetchShim = (name: string) =>
    writeShim(
      bin,
      name,
      [
        'dest=""',
        'url=""',
        'want=""',
        'for a in "$@"; do',
        "  case \"$a\" in",
        "    -o|-O) want=dest; continue ;;",
        "    http://*|https://*) url=$a ;;",
        "  esac",
        '  if [ "$want" = dest ]; then dest=$a; want=; fi',
        "done",
        `printf '%s\\n' "$url" >> ${JSON.stringify(calls)}`,
        'if [ -z "$url" ] || [ -z "$dest" ]; then echo "fetch-shim: no url/dest in: $*" >&2; exit 22; fi',
        'while IFS="$(printf \'\\t\')" read -r u p; do',
        '  [ -n "$u" ] || continue',
        '  if [ "$u" = "$url" ]; then cp "$p" "$dest" 2>/dev/null || exit 22; exit 0; fi',
        `done < ${JSON.stringify(urlmap)}`,
        'echo "fetch-shim: no fixture for $url" >&2',
        "exit 22",
      ].join("\n"),
    );
  if (!opts.noCurl) fetchShim("curl");
  fetchShim("wget");

  writeShim(
    bin,
    "uname",
    opts.kind === "macos"
      ? 'case "$1" in -m) echo arm64;; *) echo Darwin;; esac'
      : 'case "$1" in -m) echo x86_64;; *) echo Linux;; esac',
  );

  const sudoLog = join(work, "sudo-calls");
  writeFileSync(sudoLog, "");
  writeShim(bin, "sudo", `printf '%s\\n' "$*" >> ${JSON.stringify(sudoLog)}\nexit 0`);

  const aptLog = join(work, "apt-calls");
  writeFileSync(aptLog, "");
  writeShim(bin, "apt-get", `printf '%s\\n' "$*" >> ${JSON.stringify(aptLog)}\nexit 0`);

  writeShim(
    bin,
    "dpkg-query",
    [
      'if [ "${MC_DEB_INSTALLED:-0}" = "1" ] && [ "${1:-}" = "-W" ]; then',
      '  echo "install ok installed"',
      "  exit 0",
      "fi",
      "exit 1",
    ].join("\n"),
  );

  if (opts.kind === "macos") {
    const xattrLog = join(work, "xattr-calls");
    writeFileSync(xattrLog, "");
    writeShim(bin, "xattr", `printf '%s\\n' "$*" >> ${JSON.stringify(xattrLog)}\nexit 0`);
    const openLog = join(work, "open-calls");
    writeFileSync(openLog, "");
    writeShim(bin, "open", `printf '%s\\n' "$*" >> ${JSON.stringify(openLog)}\nexit 0`);
  }

  if (opts.kind === "deb") {
    writeShim(bin, "mailchat-vault", 'touch "$MC_LAUNCH_MARK"\nexit 0');
  }

  let fuseDev = "";
  if (opts.haveFuse) {
    writeShim(bin, "fusermount", "exit 0");
    fuseDev = join(work, "fuse-device");
    writeFileSync(fuseDev, "");
  }

  const osRelease = join(work, "os-release");
  writeFileSync(
    osRelease,
    opts.kind === "deb" ? 'ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME="Ubuntu 24.04"\n' : "ID=fedora\nID_LIKE=rhel\n",
  );

  if (opts.preinstall) {
    if (opts.kind === "macos") {
      mkdirSync(join(home, "Applications/mailchat-vault.app/Contents/MacOS"), { recursive: true });
    } else if (opts.kind === "appimage") {
      mkdirSync(join(home, ".local/bin"), { recursive: true });
      writeFileSync(join(home, ".local/bin/mailchat-vault.AppImage"), "old appimage", { mode: 0o755 });
      writeFileSync(join(home, ".local/bin/mailchat-vault"), "old launcher", { mode: 0o755 });
    }
  }

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    PATH: `${bin}:${process.env.PATH}`,
    HOME: home,
    TMPDIR: tmp,
    MAILCHAT_OS_RELEASE: osRelease,
    MC_LAUNCH_MARK: launchMark,
    MC_DEB_INSTALLED: opts.debInstalled ? "1" : "0",
  };
  // The runner may export XDG_DATA_HOME (it would redirect the
  // .desktop/icon installs out of the sandbox's HOME).
  delete env.XDG_DATA_HOME;
  if (opts.haveFuse) {
    env.MAILCHAT_FUSE_DEVICE = fuseDev;
  } else {
    // The probe falls back to /dev/fuse when the override is unset, and
    // hosts WITH /dev/fuse (CI runners, dev machines) would then detect
    // FUSE in a no-FUSE world — the plain launcher would be written and
    // the extract-and-run contract would fail. Point the probe at a
    // path that never exists instead.
    env.MAILCHAT_FUSE_DEVICE = join(work, "no-fuse-device");
  }

  const run = (args: string[] = [], extraEnv: Record<string, string> = {}) =>
    spawnSync("sh", [installSh, ...args], {
      encoding: "utf8",
      env: { ...env, ...extraEnv },
      timeout: 30_000,
    });

  // PATH holding only the shims plus the real tools a run needs (and
  // never curl): proves the wget fallback without depending on the host.
  const isolatedRun = (args: string[], extraEnv: Record<string, string> = {}) => {
    const iso = join(work, "isolated-bin");
    mkdirSync(iso);
    // the interpreter itself must resolve inside the isolated PATH
    const shPath = spawnSync("bash", ["-c", "command -v sh"], { encoding: "utf8" });
    if (shPath.status === 0) symlinkSync(shPath.stdout.trim(), join(iso, "sh"));
    for (const f of readdirSync(bin)) {
      if (f === "curl") continue;
      symlinkSync(join(bin, f), join(iso, f));
    }
    const realTools = [
      "sha256sum",
      "tar",
      "mktemp",
      "grep",
      "sed",
      "awk",
      "head",
      "tr",
      "cut",
      "cp",
      "mv",
      "rm",
      "chmod",
      "mkdir",
      "id",
      "cat",
      "nohup",
      "dpkg-query",
      "apt-get",
    ];
    for (const t of realTools) {
      const r = spawnSync("bash", ["-c", `command -v ${t}`], { encoding: "utf8" });
      if (r.status === 0 && !existsSync(join(iso, t))) {
        symlinkSync(r.stdout.trim(), join(iso, t));
      }
    }
    return spawnSync("sh", [installSh, ...args], {
      encoding: "utf8",
      env: { ...env, ...extraEnv, PATH: iso },
      timeout: 30_000,
    });
  };

  return {
    work,
    home,
    tmp,
    vaultDb,
    calls,
    sudoLog,
    xattrLog: join(work, "xattr-calls"),
    openLog: join(work, "open-calls"),
    env,
    run,
    isolatedRun,
  };
}

const noTempLeft = (world: World) => expect(readdirSync(world.tmp)).toHaveLength(0);
const vaultIntact = (world: World) => {
  expect(readFileSync(world.vaultDb, "utf8")).toBe("vault db fixture - must survive");
};

describe("install.sh parses", () => {
  it.skipIf(!hasSh)("sh -n (dash here) accepts the installer", () => {
    execFileSync("sh", ["-n", installSh]);
  });

  it.skipIf(!hasBash)("bash -n accepts the installer", () => {
    execFileSync("bash", ["-n", installSh]);
  });

  it.skipIf(!hasShellcheck)("shellcheck reports no warnings for install.sh", () => {
    execFileSync("shellcheck", ["--severity=warning", installSh]);
  });
});

describe("install.sh flag handling", () => {
  it.skipIf(!hasSh)("unknown flags exit 2 with usage", () => {
    const world = buildWorld({ kind: "appimage" });
    const r = world.run(["--bogus"]);
    expect(r.status).toBe(2);
    expect(r.stderr).toContain("unknown option: --bogus");
    expect(r.stderr).toContain("usage:");
  });
});

describe("install.sh deb path (Debian/Ubuntu, apt + sudo)", () => {
  it.skipIf(!hasSh)(
    "installs the .deb via sudo apt-get, verifies the checksum, keeps data, cleans temp",
    () => {
      const world = buildWorld({ kind: "deb" });
      const r = world.run(["--no-launch"]);
      expect(r.status).toBe(0);
      expect(r.stdout).toContain("Mailchat Vault 0.2.0 installed");
      expect(r.stdout).toContain("verified sha256");
      const sudo = readFileSync(world.sudoLog, "utf8");
      expect(sudo).toContain("apt-get install -y");
      expect(sudo).toMatch(/mailchat-install\.[^/]+\/mailchat-vault_0\.2\.0_amd64\.deb/);
      expect(readFileSync(world.calls, "utf8")).toContain("releases/latest");
      expect(existsSync(join(world.home, ".local/bin"))).toBe(false);
      vaultIntact(world);
      noTempLeft(world);

      // re-running is idempotent (same deb, still green)
      const again = world.run(["--no-launch"]);
      expect(again.status).toBe(0);
      expect(readFileSync(world.sudoLog, "utf8").trim().split("\n")).toHaveLength(2);
    },
  );

  it.skipIf(!hasSh)("launches the installed binary unless --no-launch", async () => {
    const world = buildWorld({ kind: "deb" });
    const mark = join(world.work, "launch-mark");
    const r = world.run([]);
    expect(r.status).toBe(0);
    await vi.waitFor(() => expect(existsSync(mark)).toBe(true), { timeout: 10_000, interval: 50 });
  });
});

describe("install.sh AppImage path (non-Debian Linux)", () => {
  it.skipIf(!hasSh)(
    "installs AppImage + launcher + desktop entry + icon; re-run replaces in place",
    () => {
      const world = buildWorld({ kind: "appimage" });
      const r = world.run(["--no-launch"]);
      expect(r.status).toBe(0);
      expect(r.stdout).toContain("Mailchat Vault 0.2.0 installed");

      const binDir = join(world.home, ".local/bin");
      const appimage = join(binDir, "mailchat-vault.AppImage");
      const wrapper = join(binDir, "mailchat-vault");
      expect(existsSync(appimage)).toBe(true);
      // the runner has no FUSE -> the wrapper must extract-and-run
      const wrapperSrc = readFileSync(wrapper, "utf8");
      expect(wrapperSrc).toContain("--appimage-extract-and-run");
      expect(wrapperSrc).toContain(appimage);
      const desktop = readFileSync(
        join(world.home, ".local/share/applications/mailchat-vault.desktop"),
        "utf8",
      );
      expect(desktop).toContain(`Exec=${wrapper}`);
      expect(desktop).toContain("Terminal=false");
      const iconMatch = desktop.match(/^Icon=(.+)$/m);
      expect(iconMatch).not.toBeNull();
      expect(existsSync(iconMatch![1])).toBe(true);
      vaultIntact(world);
      noTempLeft(world);

      // idempotent re-run, now with the upgrade-aware note
      const again = world.run(["--no-launch"]);
      expect(again.status).toBe(0);
      expect(again.stdout).toContain("existing AppImage found");
      vaultIntact(world);
      noTempLeft(world);
    },
  );

  it.skipIf(!hasSh)("uses a plain launcher when FUSE is available", () => {
    const world = buildWorld({ kind: "appimage", haveFuse: true });
    const r = world.run(["--no-launch"]);
    expect(r.status).toBe(0);
    const wrapper = readFileSync(join(world.home, ".local/bin/mailchat-vault"), "utf8");
    expect(wrapper).not.toContain("--appimage-extract-and-run");
    expect(wrapper).toContain('exec "');
  });

  it.skipIf(!hasSh)("launches through the wrapper unless --no-launch", async () => {
    const world = buildWorld({ kind: "appimage" });
    const mark = join(world.work, "launch-mark");
    const r = world.run([]);
    expect(r.status).toBe(0);
    await vi.waitFor(() => expect(existsSync(mark)).toBe(true), { timeout: 10_000, interval: 50 });
  });

  it.skipIf(!hasSh)("falls back to wget when curl is absent", () => {
    const world = buildWorld({ kind: "appimage", noCurl: true });
    const r = world.isolatedRun(["--no-launch"]);
    expect(r.status).toBe(0);
    expect(r.stdout).toContain("Mailchat Vault 0.2.0 installed");
    expect(existsSync(join(world.home, ".local/bin/mailchat-vault.AppImage"))).toBe(true);
    expect(readFileSync(world.calls, "utf8")).toContain("releases/latest");
    noTempLeft(world);
  });
});

describe("install.sh macOS path", () => {
  it.skipIf(!hasSh)("installs the .app tarball, clears quarantine, launches, keeps data", () => {
    const world = buildWorld({ kind: "macos" });
    const r = world.run([]);
    expect(r.status).toBe(0);
    expect(r.stdout).toContain("Mailchat Vault 0.2.0 installed");
    // /Applications is not writable in the sandbox -> ~/Applications
    const app = join(world.home, "Applications/mailchat-vault.app");
    expect(existsSync(join(app, "Contents/MacOS/mailchat-vault"))).toBe(true);
    expect(readFileSync(world.xattrLog, "utf8")).toContain(`-cr ${app}`);
    expect(readFileSync(world.openLog, "utf8")).toContain(app);
    vaultIntact(world);
    noTempLeft(world);
  });

  it.skipIf(!hasSh)("respects --no-launch", () => {
    const world = buildWorld({ kind: "macos" });
    const r = world.run(["--no-launch"]);
    expect(r.status).toBe(0);
    expect(r.stdout).toContain("--no-launch");
    expect(readFileSync(world.openLog, "utf8")).toBe("");
  });
});

describe("install.sh version pinning", () => {
  it.skipIf(!hasSh)("--version v0.1.0 resolves through the tag endpoint", () => {
    const world = buildWorld({ kind: "deb", pinVersion: "0.1.0" });
    const r = world.run(["--version", "v0.1.0", "--no-launch"]);
    expect(r.status).toBe(0);
    expect(r.stdout).toContain("Mailchat Vault 0.1.0 installed");
    expect(readFileSync(world.calls, "utf8")).toContain("releases/tags/v0.1.0");
    expect(readFileSync(world.sudoLog, "utf8")).toContain("mailchat-vault_0.1.0_amd64.deb");
    noTempLeft(world);
  });

  it.skipIf(!hasSh)("MAILCHAT_VERSION works without the v prefix", () => {
    const world = buildWorld({ kind: "appimage", pinVersion: "0.1.0" });
    const r = world.run(["--no-launch"], { MAILCHAT_VERSION: "0.1.0" });
    expect(r.status).toBe(0);
    expect(r.stdout).toContain("Mailchat Vault 0.1.0 installed");
    expect(readFileSync(world.calls, "utf8")).toContain("releases/tags/v0.1.0");
    noTempLeft(world);
  });

  it.skipIf(!hasSh)("refuses a draft release with a clear error", () => {
    const world = buildWorld({ kind: "appimage", pinVersion: "0.3.0", draft: true });
    const r = world.run(["--version", "v0.3.0"]);
    expect(r.status).not.toBe(0);
    expect(r.stderr).toContain("DRAFT");
    expect(existsSync(join(world.home, ".local/bin"))).toBe(false);
    noTempLeft(world);
  });

  it.skipIf(!hasSh)("rejects a --version that is not a version", () => {
    const world = buildWorld({ kind: "appimage" });
    const r = world.run(["--version", "latest@@"]);
    expect(r.status).not.toBe(0);
    expect(r.stderr).toContain("does not look like a version");
  });
});

describe("install.sh verification gate", () => {
  it.skipIf(!hasSh)(
    "a tampered checksum aborts before installing anything and cleans temp",
    () => {
      for (const tamper of ["appimage", "deb", "mac"] as const) {
        const world = buildWorld({
          kind: tamper === "deb" ? "deb" : tamper === "mac" ? "macos" : "appimage",
          tamper,
        });
        const r = world.run(["--no-launch"]);
        expect(r.status, `tamper=${tamper}`).not.toBe(0);
        expect(r.stderr, `tamper=${tamper}`).toContain("checksum mismatch");
        expect(r.stderr, `tamper=${tamper}`).toContain("NOTHING was installed");
        if (tamper !== "mac") {
          expect(readFileSync(world.sudoLog, "utf8"), `tamper=${tamper}`).toBe("");
          expect(existsSync(join(world.home, ".local/bin"))).toBe(false);
        } else {
          expect(existsSync(join(world.home, "Applications"))).toBe(false);
        }
        vaultIntact(world);
        noTempLeft(world);
      }
    },
  );

  it.skipIf(!hasSh)("a missing SHA256SUMS entry refuses to install", () => {
    const world = buildWorld({ kind: "appimage", omitSumsEntry: "appimage" });
    const r = world.run(["--no-launch"]);
    expect(r.status).not.toBe(0);
    expect(r.stderr).toContain("no entry for 'mailchat-vault_0.2.0_amd64.AppImage'");
    expect(existsSync(join(world.home, ".local/bin"))).toBe(false);
    noTempLeft(world);
  });
});

describe("install.sh uninstall", () => {
  it.skipIf(!hasSh || !hasSetsid || process.platform !== "linux")(
    "asks for confirmation; without a terminal it requires --yes",
    () => {
      const world = buildWorld({ kind: "appimage", preinstall: true });
      // setsid: no controlling terminal, deterministically no /dev/tty
      const r = spawnSync("setsid", ["sh", installSh, "--uninstall"], {
        encoding: "utf8",
        env: world.env,
        timeout: 30_000,
      });
      expect(r.status).not.toBe(0);
      expect(r.stderr).toContain("--yes");
      expect(existsSync(join(world.home, ".local/bin/mailchat-vault.AppImage"))).toBe(true);
    },
  );

  it.skipIf(!hasSh)("removes the AppImage install and keeps user data", () => {
    const world = buildWorld({ kind: "appimage", preinstall: true });
    const r = world.run(["--uninstall", "--yes"]);
    expect(r.status).toBe(0);
    expect(existsSync(join(world.home, ".local/bin/mailchat-vault"))).toBe(false);
    expect(existsSync(join(world.home, ".local/bin/mailchat-vault.AppImage"))).toBe(false);
    expect(existsSync(join(world.home, ".local/share/applications/mailchat-vault.desktop"))).toBe(false);
    expect(r.stdout).toContain("KEPT");
    vaultIntact(world);
  });

  it.skipIf(!hasSh)("removes the deb via apt and keeps user data", () => {
    const world = buildWorld({ kind: "deb", debInstalled: true });
    const r = world.run(["--uninstall", "--yes"]);
    expect(r.status).toBe(0);
    expect(readFileSync(world.sudoLog, "utf8")).toContain("apt-get remove -y mailchat-vault");
    expect(r.stdout).toContain("KEPT");
    vaultIntact(world);
  });

  it.skipIf(!hasSh)("removes the macOS app and keeps user data", () => {
    const world = buildWorld({ kind: "macos", preinstall: true });
    const r = world.run(["--uninstall", "--yes"]);
    expect(r.status).toBe(0);
    expect(existsSync(join(world.home, "Applications/mailchat-vault.app"))).toBe(false);
    expect(r.stdout).toContain("KEPT");
    vaultIntact(world);
  });
});

describe("install.sh docs contract", () => {
  it("README Quickstart and docs/download.md carry the one-liner", () => {
    expect(readmeMd).toContain("### Install in one line");
    expect(readmeMd).toContain(ONE_LINER);
    expect(downloadMd).toContain("## Install in one line");
    expect(downloadMd).toContain(ONE_LINER);
    expect(downloadMd).toContain("--uninstall");
    expect(downloadMd).toContain("--version");
  });

  it("docs tell users to read the script before piping it to a shell", () => {
    for (const doc of [readmeMd, downloadMd]) {
      expect(doc).toMatch(/[Rr]ead (the script|it) before/);
    }
  });

  it("no Windows one-liner is advertised yet (install.ps1 is a later part)", () => {
    for (const doc of [readmeMd, downloadMd]) {
      expect(doc).not.toContain("iex");
      expect(doc).not.toMatch(/install\.ps1 \|/);
    }
  });
});
