// @vitest-environment node
// Package-manager manifest kit checks (P0·04, docs/distribution.md).
//
// The kit must stay publication-ready without publishing anything:
//
//   - scripts/update-manifests.sh smoke-tested against the fixture
//     checksums file: dry run prints a diff and changes nothing;
//     --write on a temp copy rewrites version + sha256 in every
//     manifest, atomically, without touching the blocked-note
//     comments that tie submission to signing (#78)
//   - every manifest validates against its schema shape: JSON parse
//     (Scoop, Flatpak), YAML parse (winget), `bash -n` (PKGBUILD),
//     structural assertions (Homebrew cask)
//
// Real per-manager validation (brew audit, winget validate,
// flatpak-builder) needs each manager's tooling and stays an owner
// step — see docs/distribution.md.
import { describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { load as loadYaml } from "js-yaml";

const repoRoot = fileURLToPath(new URL("../..", import.meta.url));
const script = join(repoRoot, "scripts", "update-manifests.sh");
const manifestsDir = join(repoRoot, "packaging", "manifests");
const fixture = join(repoRoot, "tests", "packaging", "fixtures", "SHA256SUMS");

const VERSION = "0.2.0";
const ZERO64 = "0".repeat(64);
const SHA_DMG = "a1".repeat(32);
const SHA_MSI = "c3".repeat(32);
const SHA_DEB = "f6".repeat(32);

const kitFiles = [
  "homebrew/mailchat-vault.rb",
  "winget/acailic.mailchat-vault.yaml",
  "winget/acailic.mailchat-vault.installer.yaml",
  "winget/acailic.mailchat-vault.locale.en-US.yaml",
  "scoop/mailchat-vault.json",
  "aur/PKGBUILD",
  "flatpak/com.acailic.mailchatvault.json",
];

const read = (rel: string) => readFileSync(join(manifestsDir, rel), "utf8");

// A throwaway kit copy with the same scripts/ + packaging/ layout the
// script resolves relative to its own location, so --write never
// touches the repo's manifests.
function makeKitCopy(): string {
  const dir = mkdtempSync(join(tmpdir(), "mcv-manifests-"));
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "packaging"));
  cpSync(manifestsDir, join(dir, "packaging", "manifests"), { recursive: true });
  cpSync(script, join(dir, "scripts", "update-manifests.sh"));
  return dir;
}

function runScript(args: string[], cwd: string = repoRoot) {
  return spawnSync("bash", [join(cwd, "scripts", "update-manifests.sh"), ...args], {
    encoding: "utf8",
    cwd,
  });
}

describe("manifest kit shape (repo state)", () => {
  it("every manifest names the signing gate (#78) and carries placeholders", () => {
    // Files that reference release artifacts carry a placeholder sha256
    // (64 zeros); the two winget files without installer entries have
    // no checksum field to placehold.
    const withSha = new Set([
      "homebrew/mailchat-vault.rb",
      "winget/acailic.mailchat-vault.installer.yaml",
      "scoop/mailchat-vault.json",
      "aur/PKGBUILD",
      "flatpak/com.acailic.mailchatvault.json",
    ]);
    for (const rel of kitFiles) {
      const text = read(rel);
      expect(text, rel).toContain("#78");
      expect(text, rel).toContain("0.0.0");
      if (withSha.has(rel)) expect(text, rel).toContain(ZERO64);
    }
  });

  it("scoop manifest parses as JSON with the expected shape", () => {
    const scoop = JSON.parse(read("scoop/mailchat-vault.json"));
    expect(scoop.version).toBe("0.0.0");
    expect(scoop.hash).toMatch(/^[0-9a-f]{64}$/);
    expect(scoop.url).toContain("x64_en-US.msi");
    expect(scoop.homepage).toBe("https://github.com/acailic/mailchat-vault");
    expect(scoop.license).toBe("GPL-3.0-only");
  });

  it("flatpak manifest parses as JSON with the documented finish-args", () => {
    const flatpak = JSON.parse(read("flatpak/com.acailic.mailchatvault.json"));
    expect(flatpak["app-id"]).toBe("com.acailic.mailchatvault");
    expect(flatpak["finish-args"]).toEqual([
      "--socket=wayland",
      "--socket=fallback-x11",
      "--share=network",
      "--filesystem=xdg-download:ro",
    ]);
    expect(flatpak.modules[0].sources[0].sha256).toMatch(/^[0-9a-f]{64}$/);
  });

  it("winget manifests parse as YAML with the winget-pkgs shape", () => {
    const version = loadYaml(read("winget/acailic.mailchat-vault.yaml")) as Record<string, string>;
    expect(version.PackageIdentifier).toBe("acailic.mailchat-vault");
    expect(version.PackageVersion).toBe("0.0.0");
    expect(version.DefaultLocale).toBe("en-US");
    expect(version.ManifestType).toBe("version");

    const installer = loadYaml(
      read("winget/acailic.mailchat-vault.installer.yaml"),
    ) as Record<string, any>;
    expect(installer.PackageVersion).toBe("0.0.0");
    expect(installer.ManifestType).toBe("installer");
    expect(installer.Installers).toHaveLength(1);
    const entry = installer.Installers[0];
    expect(entry.Architecture).toBe("x64");
    expect(entry.InstallerType).toBe("wix");
    expect(entry.InstallerUrl).toContain("mailchat-vault_0.0.0_x64_en-US.msi");
    // The 64-zero placeholder parses as a YAML number; real checksums
    // (hex with letters) parse as strings.
    expect(String(entry.InstallerSha256)).toMatch(/^[0-9a-f]{64}$/);

    const locale = loadYaml(
      read("winget/acailic.mailchat-vault.locale.en-US.yaml"),
    ) as Record<string, string>;
    expect(locale.ManifestType).toBe("defaultLocale");
    expect(locale.PackageName).toBe("Mailchat Vault");
    expect(locale.License).toBe("GPL-3.0-only");
  });

  it("PKGBUILD parses with bash -n and states why it is binary-based", () => {
    const r = spawnSync("bash", ["-n", join(manifestsDir, "aur", "PKGBUILD")]);
    expect(r.status).toBe(0);
    const pkgbuild = read("aur/PKGBUILD");
    expect(pkgbuild).toContain("pkgname=mailchat-vault-bin");
    expect(pkgbuild).toContain("pkgver=0.0.0");
    expect(pkgbuild).toMatch(/binary release|source build/);
  });

  it("homebrew cask carries the structural stanzas a cask needs", () => {
    const cask = read("homebrew/mailchat-vault.rb");
    expect(cask).toMatch(/^cask "mailchat-vault" do$/m);
    expect(cask).toMatch(/^  version "0\.0\.0"$/m);
    expect(cask).toMatch(/^  sha256 "[0-9a-f]{64}"$/m);
    expect(cask).toContain("mailchat-vault_#{version}_universal.dmg");
    expect(cask).toMatch(/^  auto_updates (true|false)$/m);
    expect(cask).toMatch(/^  app "mailchat-vault\.app"$/m);
  });
});

describe("update-manifests.sh smoke test", () => {
  it("dry run prints a diff and changes nothing", () => {
    const kit = makeKitCopy();
    try {
      const before = snapshotDir(join(kit, "packaging", "manifests"));
      const r = runScript(["--version", VERSION, "--checksums", fixture], kit);
      expect(r.status, r.stderr).toBe(0);

      // The diff names every rewritten manifest and the new values.
      for (const rel of kitFiles) expect(r.stdout).toContain(`+++ b/${rel}`);
      expect(r.stdout).toContain(`+  version "${VERSION}"`);
      expect(r.stdout).toContain(`+PackageVersion: ${VERSION}`);
      expect(r.stdout).toContain(`+pkgver=${VERSION}`);
      expect(r.stdout).toContain(`+  "version": "${VERSION}",`);
      expect(r.stdout).toContain(`+    InstallerSha256: "${SHA_MSI}"`);
      expect(r.stdout).toContain(`+  "hash": "${SHA_MSI}`);
      expect(r.stdout).toContain(`+  sha256 "${SHA_DMG}"`);
      expect(r.stdout).toContain(`+sha256sums_x86_64=('${SHA_DEB}')`);
      expect(r.stdout).toContain("dry run");

      // And the manifests are byte-for-byte untouched.
      expect(snapshotDir(join(kit, "packaging", "manifests"))).toEqual(before);
    } finally {
      rmSync(kit, { recursive: true, force: true });
    }
  });

  it("--write rewrites version and sha256 in every manifest on a temp copy", () => {
    const kit = makeKitCopy();
    try {
      const r = runScript(
        ["--version", VERSION, "--checksums", fixture, "--write"],
        kit,
      );
      expect(r.status, r.stderr).toBe(0);
      expect(r.stdout).toContain(`updated: ${kitFiles[0]}`);

      const kitRead = (rel: string) =>
        readFileSync(join(kit, "packaging", "manifests", rel), "utf8");

      for (const rel of kitFiles) {
        const text = kitRead(rel);
        expect(text, rel).not.toContain("0.0.0");
        expect(text, rel).not.toContain(ZERO64);
        expect(text, rel).toContain("#78"); // blocked note survives the rewrite
      }

      const cask = kitRead("homebrew/mailchat-vault.rb");
      expect(cask).toContain(`version "${VERSION}"`);
      expect(cask).toContain(`sha256 "${SHA_DMG}"`);
      // The cask URL interpolates the version stanza (#{version}), so it
      // needs no rewrite of its own.
      expect(cask).toContain("mailchat-vault_#{version}_universal.dmg");

      const installer = loadYaml(
        kitRead("winget/acailic.mailchat-vault.installer.yaml"),
      ) as Record<string, any>;
      expect(installer.PackageVersion).toBe(VERSION);
      expect(installer.MinimumOSVersion).toBe("10.0.19041.0"); // not clobbered by the version rewrite
      expect(installer.Installers[0].InstallerSha256).toBe(SHA_MSI);
      expect(installer.Installers[0].InstallerUrl).toContain(
        `mailchat-vault_${VERSION}_x64_en-US.msi`,
      );

      const scoop = JSON.parse(kitRead("scoop/mailchat-vault.json"));
      expect(scoop.version).toBe(VERSION);
      expect(scoop.hash).toBe(SHA_MSI);
      expect(scoop.url).toContain(`v${VERSION}/`);

      const pkgbuild = kitRead("aur/PKGBUILD");
      expect(pkgbuild).toContain(`pkgver=${VERSION}`);
      expect(pkgbuild).toContain(`'${SHA_DEB}'`);
      expect(spawnSync("bash", ["-n", join(kit, "packaging", "manifests", "aur", "PKGBUILD")]).status).toBe(0);

      const flatpak = JSON.parse(kitRead("flatpak/com.acailic.mailchatvault.json"));
      expect(flatpak.modules[0].sources[0].sha256).toBe(SHA_DEB);
      expect(flatpak.modules[0].sources[0].url).toContain(`v${VERSION}/`);
    } finally {
      rmSync(kit, { recursive: true, force: true });
    }
  });

  it("rejects a checksums file that misses a needed asset", () => {
    const kit = makeKitCopy();
    try {
      const r = runScript(
        ["--version", "9.9.9", "--checksums", fixture],
        kit,
      );
      expect(r.status).toBe(1);
      expect(r.stderr).toContain("mailchat-vault_9.9.9_universal.dmg");
    } finally {
      rmSync(kit, { recursive: true, force: true });
    }
  });
});

// Map of relpath -> { size, mtimeMs } for "nothing was written" checks.
function snapshotDir(dir: string): Record<string, { size: number; mtimeMs: number }> {
  const snap: Record<string, { size: number; mtimeMs: number }> = {};
  for (const rel of readdirSync(dir, { recursive: true }) as string[]) {
    const full = join(dir, rel);
    const st = statSync(full);
    if (st.isFile()) snap[rel] = { size: st.size, mtimeMs: st.mtimeMs };
  }
  return snap;
}
