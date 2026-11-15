# Native verification runbook (G1, A14 — macOS and Windows)

Status: **runnable substrate, zero recorded native runs** (2026-11-15). Plan gate G1 requires the install → import → find → export → backup/restore → upgrade → uninstall journey on real native hosts per OS, with every run bound to the artifact **SHA-256**, **tester**, and **date**. This runbook is the exact procedure; the evidence templates it fills live in [`docs/evidence/native-verification-templates/`](evidence/native-verification-templates/README.md). The blocker tracking the missing hosts is [#81](https://github.com/acailic/mailchat-vault/issues/81); the hosted-runner alternative waits on the billing fix in [#80](https://github.com/acailic/mailchat-vault/issues/80). Neither can run on the Linux fleet worker — that is the point of native verification ([validation and release](validation-and-release.md#platform-and-release-matrix): do not assume Linux E2E covers macOS/Windows).

**Nothing here may be faked**: if a step cannot run, record it as *not run* with the reason. A skipped leg is never certification.

## What a native run must cover

Two halves, both required for A14/G1:

1. **Automated smoke** (the scripts below): install → launch → archive created → upgrade (Windows) → uninstall-keeps-archive, against the exact published or candidate bytes, with the machine-readable evidence file attached.
2. **Manual journey** (the template checklist): import a synthetic export → find it with search → export → backup → restore → reinstall-reopens-it, plus the platform observations the smoke cannot assert (Gatekeeper/SmartScreen dialogs, Keychain/Credential Manager behavior, media preview, VoiceOver/NVDA spot check). Use synthetic fixtures only — never a real archive.

## Machines and owners (open — record in #81)

| OS | Route | Machine | Owner | Status |
| --- | --- | --- | --- | --- |
| macOS | personal Mac (Apple Silicon or Intel — the dmg is universal, record which) | _to name_ | _to name_ | not run |
| macOS | hosted `macos-latest` runner (after #80 billing fix) — drive the same commands below | — | — | blocked on #80 |
| Windows 10/11 | personal Windows box with WebView2 | _to name_ | _to name_ | not run |
| Windows | hosted `windows-latest` runner — *Actions → Windows smoke → Run workflow* on the tag (drives `scripts/windows-smoke.ps1` end to end, uploads the logs) | — | — | blocked on #80 (zero dispatches recorded to date — D11 amendment) |

## macOS — exact commands

Prerequisites: any stock Mac (nothing preinstalled beyond the OS), a repo checkout of the tag being verified, `gh` authenticated for `--release` mode.

```sh
git clone https://github.com/acailic/mailchat-vault.git && cd mailchat-vault
git checkout v0.2.0                      # the tag being verified (substitute as needed)

# Against a PUBLISHED release (preferred — exactly what users download):
scripts/release/smoke-macos.sh --release v0.2.0 --tester "Your Name"

# Against artifacts on disk (e.g. built locally with
#   npm run tauri build -- --target universal-apple-darwin --bundles app,dmg )
scripts/release/smoke-macos.sh --dir target/universal-apple-darwin/release/bundle/dmg \
  --skip-updater --tester "Your Name"
```

The run prints `RESULT: PASS|FAIL`, and writes two files under
`target/release/smoke-logs/` (or `--logs DIR`): the full log and
`macos-<stamp>.json` — the machine-readable evidence record (result,
host, tester, UTC timestamps, commit, per-check results, and the
SHA-256 of every artifact smoked). Attach **both** to the evidence
template. Until a real release exists, the `--dir` route against a
locally built dmg is the honest candidate check.

For the full journey also run `scripts/dmg-smoke-test.sh <dmg>` (headless `--version`/`--help`) and the release matrix column `scripts/release/matrix-macos.sh` if the host has node 22 + rust stable ([release.md](release.md#one-command-validation-matrix-m64)).

## Windows — exact commands

Prerequisites: 64-bit Windows 10/11 with the WebView2 runtime (stock on current installs), PowerShell 7 (`pwsh`), `gh` authenticated, and — only for the upgrade phases — a Rust toolchain (`-BuildInspect` builds the digest tool).

```powershell
git clone https://github.com/acailic/mailchat-vault.git; cd mailchat-vault
git checkout v0.2.0                      # the tag being verified (substitute as needed)

# fresh + upgrade phases against the PUBLISHED release (preferred):
pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.2.0 -OldTag v0.1.0 `
    -BuildInspect -AllowDataWipe -Tester "Your Name"

# fresh phases only (no Rust toolchain needed):
pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.2.0 -AllowDataWipe -Tester "Your Name"
```

The console ends with `RESULT: PASS|FAIL`; the run writes
`target\release\verify\<tag>\windows-smoke-logs\` containing
`summary.txt` and `summary.json` (same evidence schema: result, host,
tester, UTC timestamps, commit, per-phase results, SHA-256 of every
installer smoked — new and old). Attach both to the evidence template.

**Data-safety note:** the smoke wipes `%APPDATA%\com.acailic.mailchatvault` between phases; `-AllowDataWipe` is the explicit consent switch. Never run it on a machine whose real archive lives there.

The alternative hosted route is the manually dispatched **Windows smoke** workflow (Actions tab) — it drives the same four phases and uploads the logs; its zero dispatch record to date is the D11 gap this task tracks. Either route satisfies the automated half; the template's manual journey still needs a human at the machine.

## Recording the evidence

Fill the per-OS template ([macOS](evidence/native-verification-templates/macos.md), [Windows](evidence/native-verification-templates/windows.md)) in the same change as the run: copy the JSON evidence (or attach it), paste the artifact SHA-256s it records, and walk the journey checklist. File the completed template under `docs/evidence/` with a date suffix and link it from the [support matrix](support-matrix.md) row for [#81](https://github.com/acailic/mailchat-vault/issues/81) — a native run without the recorded binding does not count.

## Self-checks (any machine, incl. the Linux fleet worker)

`npm test` includes `tests/release-smoke-selfcheck.test.ts`, which statically guards this substrate: bash syntax (`bash -n`) and shellcheck (when installed) on the smoke scripts, PowerShell parse checks (when `pwsh` is installed), the off-platform refusals, and the evidence-contract markers (SHA-256 binding, tester, JSON output, `RESULT:` line) in the scripts, this runbook, and the templates. It cannot and does not substitute for a native run.
