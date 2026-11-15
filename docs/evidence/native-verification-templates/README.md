# A14 native-verification evidence templates

Fill one template per OS per candidate and file it under `docs/evidence/`
with a date suffix (e.g. `a14-macos-v0.2.0-2026-11-21.md`); link the
file from the [support matrix](../../support-matrix.md) row for
[#81](https://github.com/acailic/mailchat-vault/issues/81). The
procedure that produces these numbers is the
[native verification runbook](../../native-verification.md) — start
there.

Rules for the record (the G1 gate enforces them):

- Every automated half must be **bound to the artifact SHA-256** copied
  from the smoke run's JSON evidence file — never a bare "it passed".
- Every run records **tester** and **date (UTC)**.
- A step that could not run is recorded as **not run** with the reason —
  never silently omitted, never backfilled, never simulated.
- Manual journey steps use **synthetic fixtures only** (e.g. the demo
  archive or a synthetic export); never a real archive.
- The signed-candidate half of A14 stays open until signing exists
  ([#78](https://github.com/acailic/mailchat-vault/issues/78)); record
  it as such on unsigned candidates.

Templates:

- [`macos.md`](macos.md) — macOS (universal dmg; Apple Silicon or Intel, record which)
- [`windows.md`](windows.md) — Windows x64 (msi + NSIS)
