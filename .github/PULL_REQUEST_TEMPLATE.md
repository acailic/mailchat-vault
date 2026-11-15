<!--
Per-PR local verification protocol (interim, while Actions is down for
billing — see docs/testing.md → "Interim per-PR verification while CI is
down"). While GitHub Actions starts no jobs, red or absent checks are not
code verdicts: the gate is the Local verification block below, filled in
honestly from a real run on the PR branch. Never backfill a verdict that
did not happen — a pending leg stays pending.
-->

## Summary

<!-- What user problem this solves and how; one short paragraph. -->

## Local verification (interim protocol — required while Actions is down)

Protocol: [docs/testing.md → Interim per-PR verification](../docs/testing.md#interim-per-pr-verification-while-ci-is-down).
Tiers: **A** = any machine · **B** = Linux webkit2gtk machine (or the release suite container) · **C** = packaged smoke.

Branch/commit verified: `<sha>`

| Tier | Suite | Command | Machine class | Result |
| --- | --- | --- | --- | --- |
| A | TypeScript type-check | `npm run check` | <!-- e.g. container/any --> | <!-- pass (clean) / FAIL / not run --> |
| A | Frontend unit tests | `npm test` | <!-- machine --> | <!-- e.g. pass: 563 passed / FAIL / pending --> |
| A | vault-core logic tests | `cargo test` | <!-- machine --> | <!-- pass: N passed / FAIL / pending --> |
| B | Rust formatting | `cargo fmt --check` | <!-- machine --> | <!-- pass / FAIL / pending --> |
| B | Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | <!-- machine --> | <!-- pass / FAIL / pending --> |
| B | Full workspace tests | `cargo test --workspace` | <!-- machine --> | <!-- pass: N passed / FAIL / pending --> |
| B | End-to-end suite | `xvfb-run -a npm run test:e2e` | <!-- machine --> | <!-- pass: N passed, M skipped / FAIL / pending --> |
| C | Packaged smoke | `npm run tauri build` + `scripts/release/smoke-linux.sh` (+ relevant specs) | <!-- machine --> | <!-- pass / FAIL / pending --> |

Minimum tiers for this change class (per the protocol's change-class
table): <!-- e.g. "frontend change → tier A + vitest" · "UI shell/CSP/packaging → tier B e2e + tier C" -->

Not run here and why (each pending leg needs a named machine class or a
restored CI): <!-- e.g. "tier B/C legs: no webkit2gtk on this host — pending a tier B machine" -->

## User-facing changes

<!-- None / describe; note CHANGELOG.md updates. -->
