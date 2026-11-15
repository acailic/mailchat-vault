# A14 native verification — macOS — <tag / candidate> — TEMPLATE

> Copy to `docs/evidence/a14-macos-<tag>-<date>.md` and fill every
> field. Procedure: [native verification runbook](../../native-verification.md).
> Synthetic fixtures only; record **not run** + reason for anything you
> could not do — that is an acceptable answer, a blank is not.

## Run metadata

| Field | Value |
| --- | --- |
| Candidate / tag | — fill (e.g. `v0.2.0` at commit `—`) — |
| Route | — personal Mac / hosted `macos-latest` runner — |
| Tester (name) | — fill — |
| Date (UTC) | — fill — |
| Host | — model, macOS version, arch (Apple Silicon arm64 / Intel x86_64; the dmg is universal — record the one you used) — |
| Smoke command run | — paste the exact `scripts/release/smoke-macos.sh …` line — |
| Smoke evidence JSON | — paste `macos-<stamp>.json` contents or attach it — |
| Smoke result | — PASS/FAIL + passed/failed counts — |

## Artifacts (SHA-256 copied from the smoke evidence JSON)

| Artifact | SHA-256 |
| --- | --- |
| `mailchat-vault_<v>_universal.dmg` | — fill — |
| `mailchat-vault_<v>_universal.app.tar.gz` (updater) | — fill or *not present (local build, `--skip-updater`)* — |

## Automated smoke checks (from the JSON `checks` list)

| Check | Result |
| --- | --- |
| dmg attaches read-only / contains exactly one .app / detaches cleanly | — PASS/FAIL — |
| updater archive valid, version matches | — PASS/FAIL / skipped — |
| Info.plist: bundle id / version / executable | — PASS/FAIL — |
| universal binary (arm64 + x86_64) | — PASS/FAIL — |
| ad-hoc code signature present | — PASS/FAIL — |
| app launches, stays alive 20 s, quits cleanly | — PASS/FAIL — |
| `dmg-smoke-test.sh` `--version`/`--help` (if run) | — PASS/FAIL/not run — |

## G1 journey (manual, in the installed app)

| Step | Expected | Observed |
| --- | --- | --- |
| Install (dmg → drag to /Applications) | app lands in /Applications | — fill — |
| First launch past Gatekeeper (unsigned round) | the documented one-time prompt; `xattr -cr` or right-click → Open | — fill (which path you used) — |
| First boot creates the archive | `~/Library/Application Support/com.acailic.mailchatvault/` with `vault.db` | — fill (exact path seen) — |
| Import a synthetic export | import preview honest, bounded; demo/sample archive or synthetic mbox | — fill — |
| Find | search finds the imported corpus; snippets and context work | — fill — |
| Export (CSV/JSON/MBOX) | export round-trips | — fill — |
| Backup | Settings backup produces a verified snapshot | — fill — |
| Restore | restore from that snapshot; previous vault recoverable | — fill — |
| Upgrade | install the next candidate over it; archive survives and reopens (schema migration on open) | — fill or *not run (no newer candidate)* — |
| Uninstall (delete app) | archive **retained** by default in Application Support | — fill — |
| Reinstall → reopens it | reinstalling the same app reopens the existing archive | — fill — |

## Platform observations (A14 macOS column)

| Observation | Result |
| --- | --- |
| Keychain available/unavailable behavior | — fill (credentials never silently plaintext when Keychain unavailable) — |
| Permissions/dialogs (first-launch, notifications, media open) | — fill — |
| Media preview in the packaged app (image/audio/video sample) | — fill (codec behavior on this Mac) — |
| VoiceOver spot check (window/landmark/result announcement) | — fill — |
| Signed-candidate half | **not applicable yet — no signing exists → #78** |

## Not run / limitations

— list every step you could not perform, with the reason —
