# Windows installer smoke tests (M6.2)

Automated Windows release quality: the installers must install, the app
must launch and stay alive, the uninstallers must remove the app
**without ever touching the user's archive**, and an in-place upgrade
from an older release must carry the archive across intact.

Everything below runs on a real `windows-latest` GitHub runner through
[`.github/workflows/windows-smoke.yml`](../../.github/workflows/windows-smoke.yml)
(manually dispatched — repo workflows are currently disabled by
decision, so start it from the *Actions* tab) and is driven by
[`scripts/windows-smoke.ps1`](../../scripts/windows-smoke.ps1).

## The contract

| Phase | What it proves |
| --- | --- |
| `msi-fresh` | The WiX `.msi` installs, the app launches and stays alive ≥ 20 s, the first launch creates `vault.db` in `%APPDATA%\com.acailic.mailchatvault`, the uninstall removes the app **and leaves `vault.db` alone**. |
| `nsis-fresh` | The same contract through the NSIS `-setup.exe` (silent `/S`). |
| `msi-upgrade` | Installing the **old** release (default: the published `v0.1.0`), seeding its data dir with the frozen fixture archive, then installing the **new** `.msi` over it: the app upgrades in place, opens the archive (running any pending schema migration), and the content is byte-for-byte the same corpus — message counts, conversation counts, FTS documents and a `'vault'` full-text query must all be identical before and after; only the schema version may advance. Uninstalling afterwards still leaves the archive. |
| `nsis-upgrade` | The same upgrade contract through the NSIS installer. |

"Archive intact" is asserted with the read-only digest tool
[`crates/vault-core/examples/smoke_inspect.rs`](../../crates/vault-core/examples/smoke_inspect.rs):
`messages`, `identities`, `occurrences`, `conversations`,
`source_instances`, `body_bytes`, `fts_docs` and `fts_vault_hits` must be
identical across the upgrade, and `fts_vault_hits > 0` proves the
full-text index is still searchable after a migration.

## The frozen fixture

`fixtures/vault-v0.1.0.db` is an archive at the **v0.1.0 schema (11)**
with the six built-in demo messages, generated from the `v0.1.0` tag by
[`crates/vault-core/examples/make_smoke_fixture.rs`](../../crates/vault-core/examples/make_smoke_fixture.rs).
It must stay frozen: the old (v0.1.0) app refuses newer-schema databases
by design (safe migration), so regenerating it from a newer workspace
would break the premise of the whole upgrade test. The generator refuses
to emit anything but schema 11, and
[`crates/vault-core/tests/smoke_fixture.rs`](../../crates/vault-core/tests/smoke_fixture.rs)
pins the fixture's contents in CI so drift is caught early.

## Running

From the repo (on a Windows machine with the bundles built):

```powershell
cargo build --release -p vault-core --example smoke_inspect
./scripts/windows-smoke.ps1 `
  -NewMsi target\release\bundle\msi\mailchat-vault_0.1.0_x64_en-US.msi `
  -NewExe target\release\bundle\nsis\mailchat-vault_0.1.0_x64-setup.exe `
  -OldMsi C:\downloads\mailchat-vault_0.1.0_x64_en-US.msi `
  -OldExe C:\downloads\mailchat-vault_0.1.0_x64-setup.exe `
  -OldLabel v0.1.0 `
  -FixtureDb tests\windows-smoke\fixtures\vault-v0.1.0.db `
  -InspectExe target\release\examples\smoke_inspect.exe `
  -AllowDataWipe
```

On GitHub: *Actions → Windows smoke → Run workflow*, pick the ref to
build, the old tag to upgrade from (default `v0.1.0`), and optionally a
test-only `version_override`. The run uploads `windows-smoke-logs`
(msiexec logs, app stdout/stderr, before/after digests, summary).

## Notes & known nuances

- **The script deletes `%APPDATA%\com.acailic.mailchatvault`** before
  every phase and at the end. It refuses to start when `vault.db`
  already exists there unless you pass `-AllowDataWipe`, so a local run
  cannot silently destroy your real archive; CI passes the switch
  because its runner is disposable.
- **Same-version MSI upgrade (exit 1638)**: WiX refuses to install the
  same product version over itself; the script detects msiexec's 1638
  and falls back to uninstall-then-install, logging a **REINSTALL**
  instead of an upgrade. A true versioned upgrade happens whenever the
  built version differs from the installed one (e.g. run the workflow
  with `version_override` set while the tree still carries the old
  version, or dispatch it on the next version tag).
- **NSIS same-version** overwrite is a true in-place upgrade — no
  fallback needed.
- The NSIS uninstaller detaches even when silent; the script polls for
  the install dir to disappear.
- The Windows runners have the WebView2 runtime preinstalled, so the
  app launches headlessly; "works" means *stays alive 20 s + the archive
  opened* (same bar as the Linux `xvfb-run` smoke in
  [docs/release.md](../../docs/release.md)), not a UI walkthrough.
