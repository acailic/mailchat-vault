# A14 native verification — Windows x64 — <tag / candidate> — TEMPLATE

> Copy to `docs/evidence/a14-windows-<tag>-<date>.md` and fill every
> field. Procedure: [native verification runbook](../../native-verification.md).
> Synthetic fixtures only; record **not run** + reason for anything you
> could not do — that is an acceptable answer, a blank is not.

## Run metadata

| Field | Value |
| --- | --- |
| Candidate / tag | — fill (e.g. `v0.2.0` at commit `—`) — |
| Old tag upgraded from | — fill (e.g. `v0.1.0`) or *fresh phases only* — |
| Route | — personal Windows box / hosted `windows-latest` runner (workflow run link) — |
| Tester (name) | — fill — |
| Date (UTC) | — fill — |
| Host | — Windows version/build, x64, WebView2 runtime present — |
| Smoke command run | — paste the exact `windows-smoke-from-release.ps1 …` line (or the workflow dispatch link) — |
| Smoke evidence JSON | — paste `windows-smoke-logs\summary.json` contents or attach it — |
| Smoke result | — PASS/FAIL + per-phase results — |

## Artifacts (SHA-256 copied from the smoke evidence JSON)

| Artifact | SHA-256 |
| --- | --- |
| `mailchat-vault_<v>_x64_en-US.msi` (new) | — fill — |
| `mailchat-vault_<v>_x64-setup.exe` (new) | — fill — |
| old `.msi` / `.exe` (upgrade phases) | — fill or *not used* — |
| fixture (`vault-v0.1.0.db`, schema 11) | — fill (must be the frozen fixture) — |

## Automated smoke phases (from the JSON `phases` list)

| Phase | Result |
| --- | --- |
| `msi-fresh` (install → alive 20 s → `vault.db` created → uninstall keeps archive) | — PASS/FAIL — |
| `nsis-fresh` (same through the NSIS setup `/S`) | — PASS/FAIL — |
| `msi-upgrade` (old + frozen fixture → new msi over it → digests identical, schema may advance, FTS live) | — PASS/FAIL / not run — |
| `nsis-upgrade` (same through NSIS) | — PASS/FAIL / not run — |

## G1 journey (manual, in the installed app)

| Step | Expected | Observed |
| --- | --- | --- |
| Install (msi and/or NSIS) past SmartScreen | the documented *More info → Run anyway* path | — fill — |
| First boot creates the archive | `%APPDATA%\com.acailic.mailchatvault\vault.db` | — fill (exact path seen) — |
| Import a synthetic export | import preview honest, bounded; demo/sample archive or synthetic mbox | — fill — |
| Find | search finds the imported corpus; snippets and context work | — fill — |
| Export (CSV/JSON/MBOX) | export round-trips | — fill — |
| Backup | Settings backup produces a verified snapshot | — fill — |
| Restore | restore from that snapshot; previous vault recoverable | — fill — |
| Upgrade (in place) | archive survives, reopens with migration on open; content identical | — covered by the upgrade phases above — |
| Uninstall (both installers) | archive **retained** by default in `%APPDATA%` | — covered by the fresh phases above — |
| Reinstall → reopens it | reinstalling after uninstall reopens the existing archive | — fill — |

## Platform observations (A14 Windows column)

| Observation | Result |
| --- | --- |
| Credential Manager available/unavailable behavior | — fill (no silent plaintext when unavailable) — |
| SmartScreen flow (installer + first launch) | — fill (matches the documented click path) — |
| File locks / path rules (archive open under two launches; single-instance behavior) | — fill — |
| Media preview in the packaged app (image/audio/video sample) | — fill — |
| NVDA spot check (window/landmark/result announcement) | — fill — |
| Signed-candidate half | **not applicable yet — no signing exists → #78** |

## Not run / limitations

— list every step you could not perform, with the reason —
