# Verification record — CI-down rerun & 2026-11-14 UI wave packaged-smoke debt (2026-11-14)

Status: **tier A legs complete in this container; tier B/C legs prepared and pending their machine class**.
Actions has started no jobs since 2026-11-14 (account spending limit — a billing action, tracked in #102; the permanent execution-route decision is #80). Until it is lifted, the operative gate is the [interim per-PR local verification protocol](../testing.md#interim-per-pr-verification-while-ci-is-down). This record is the post-merge snapshot it demands: every suite that can run in the build container was run on the merged tree (`main` at `ff13774`, the `merge/all-branches-20261114` result), and every leg this container cannot run is listed with the exact command and machine class that retires it.

## Matrix

| Leg | Ran? | Command | Machine class | Result |
| --- | --- | --- | --- | --- |
| Full-suite rerun — type-check | **ran** | `npm run check` | container (any tier A) | **pass** — clean `tsc --noEmit` |
| Full-suite rerun — frontend units | **ran** | `npm test` | container (any tier A) | **pass** — 596 tests / 18 files, 0 failed (the tree grew past the 563 noted at planning time) |
| Full-suite rerun — frontend build | **ran** | `npm run build` | container (any tier A) | **pass** — `tsc --noEmit` + vite production build in 461 ms |
| Full-suite rerun — vault-core logic tests | **attempted, blocked by environment** | `cargo test` | container (tier A) | **fail (environment, not code)** — rustup minimal profile installed user-locally (`rustc 1.100.0` under `$HOME`); every build-script crate fails `error: linker \`cc\` not found` — the runner ships no C toolchain and no root to install one. Debt data, not a code verdict: rerun on any tier A machine with a C toolchain. |
| Full-suite rerun — fmt / clippy | pending | `cargo fmt --check` · `cargo clippy --workspace --all-targets -- -D warnings` | tier B (or any tier A machine with a C toolchain — see cargo leg above) | not run here — same missing-toolchain cause |
| Full-suite rerun — workspace tests | pending | `cargo test --workspace` | tier B (needs the webkit2gtk stack for the `mailchat-vault` crate) | not run here |
| Full-suite rerun — e2e | pending | `npm run build && cargo build -p mailchat-vault --features tauri/custom-protocol && xvfb-run -a npm run test:e2e` | tier B | not run here — no WebKitWebDriver/webkit2gtk/display in this container |
| #87 whole-app scroll fix — binary check | pending | `scripts/release/build-linux.sh && scripts/release/smoke-linux.sh` (window maps + stays alive; then the UI checklist walk in release.md) | tier C | not run here — needs docker or a Linux desktop with the packaging stack |
| #88 tabs + hideable sidebar — e2e | pending | `xvfb-run -a npm run test:e2e` (workspace-layout + tail-polish specs cover layout states) | tier B | not run here |
| #89 rendered email preview — packaged smoke | **prepared**: new spec `30-mail-rendered-preview` | `npm run build && cargo build -p mailchat-vault --features tauri/custom-protocol && MAILCHAT_E2E_ONLY=rendered xvfb-run -a npm run test:e2e` | tier B (the spec self-skips unless the app serves the packaged `tauri:` origin) | not run here — spec written to the harness contract, `node --check`-validated, exports `run(ctx)`, skips with a named reason when the binary/WebDriver/packaged origin is absent |
| #90 ask collect-links — workspace tests | pending | `cargo test --workspace` | tier B (its vault-core suites ran locally only at merge time) | not run here — same cargo cause |

## What this run executed (for the record)

- `rustup` minimal profile, user-locally (no sudo needed) → `rustc 1.100.0 (0ef1bc386 2026-11-05)`, then `cargo test` at the repo root: fails at the first build-script crates with `error: linker \`cc\` not found` (no C compiler on this runner, none installable without root). The root-`cargo test` tier A promise itself — vault-core only, per workspace `default-members` — stays meaningful on any machine with a C toolchain.
- Gates on the merged tree: `npm run check` clean; `npm test` 596/596; `npm run build` green.
- The new spec `tests/e2e/specs/30-mail-rendered-preview.e2e.mjs` + fixture `tests/e2e/fixtures/rendered-email.mbox` (synthetic): import an HTML mail, open Rendered mode, prove inside the sandboxed srcdoc iframe that the `data:` image decodes and occupies layout and the link click navigates nothing (frame stays on `about:srcdoc`; app window and reading pane unchanged). Validated syntactically (`node --check`, module import) — its first real execution needs a tier B machine and may need small first-run fixes there.

## Pre-existing debt noticed, deliberately not fixed here

- The host-only keyring flakes noted in #89 stay as recorded debt (out of scope per the protocol: record, don't fix pre-existing failures inside a verification run).

## Retiring the pending legs

A tier B machine (Linux webkit2gtk stack + webkit2gtk-driver + `tauri-driver` + `xvfb-run`, or the release suite container) retires every tier B row with the exact commands above; a tier C machine (docker or a Linux desktop with the packaging stack) retires #87. Alternatively, restoring Actions billing (GitHub → Settings → Billing and plans) re-enables `.github/workflows/ci.yml` unchanged — the workflow files were not modified by this change.
