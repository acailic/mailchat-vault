# Merged UI wave (#87, #88, #89) — local verification record (2026-11-15)

Status: **complete for every check this environment can run; e2e, packaged-binary, Docker, workspace-wide Rust, and macOS checks remain [Blocked verification](../README.md#authority-and-status)** (a check could not establish application behavior). Filed against [#102](https://github.com/acailic/mailchat-vault/issues/102); CI itself is billing-blocked since 2026-11-14 and nothing here changes that.

## Verified tree and environment

- Tree: `main` at `7978573` ("test(e2e): settle before the nav-restore click in workspace-layout (#129)", cloned 2026-11-15). This is a descendant of `ffb9229` (`merge/all-branches-20261114`), 14 commits further; all three verified PR commits (`a7759a1`, `75f15e3`, `3e86afc`) are ancestors of the verified tree (`git merge-base --is-ancestor` checked for each). Suites therefore also cover everything that landed between `ffb9229` and `7978573` (#119, #122, #124–#131).
- Runner limits: **no Docker daemon, no sudo, no WebKitGTK system libraries, and no system C toolchain** — no `cc`/`gcc`/`clang`, no `ld`, no `ar` (only `gcc-12-base` runtime packages; no apt indexes for rootless `.deb` extraction).
- Rust toolchain (user-local, `$HOME`): rustup `stable` — cargo 1.100.0 / rustc 1.100.0. Because the runner has no C toolchain, the build used shims: `zig cc` 0.17.1 (official `zig-x86_64-linux-0.17.1.tar.xz`, sha256 `3581863e…b17a` verified) as the C compiler and linker for `x86_64-unknown-linux-gnu`, plus `llvm-ar`/`zranlib` from rustup's `llvm-tools-preview` for static archives. The bundled SQLite (`rusqlite` "bundled") was compiled from source by this toolchain — same source, a different C compiler than CI's gcc; that substitution is the one caveat on the Rust numbers below.

## What was verified, per merged change

| PR | Merge SHA | Change | Local suites covering it | Result |
| --- | --- | --- | --- | --- |
| #88 | `a7759a1` | Workspace Search/Ask tabs + hideable desktop sidebar (`index.html`, `state.ts`, `workspace.ts`, `main.ts`, `app.css`, new e2e spec `16-workspace-layout`) | `npm run check`, `npm test` (includes the `state.test.ts` / `main.test.ts` suites added with the change), `npm run build` | **pass** — e2e spec not run (blocked, below) |
| #89 | `75f15e3` | Email rendered preview, Rendered/Raw toggle (`render2.ts`, `viewmodel.ts`, `app.css`, `crates/vault-core/src/mail.rs` with inline unit tests) | `npm run check`, `npm test` (`render2.test.ts`, `viewmodel.test.ts`), `npm run build`, `cargo test -p vault-core` (lib unittests cover `mail.rs`) | **pass** — packaged smoke not run (blocked, below) |
| #87 | `3e86afc` | CSS guard locking the document against whole-app phantom scroll (`app.css` only) | `npm test` (render suites exercise the stylesheet-dependent DOM), `npm run build` (CSS compiles into the production bundle) | **pass** — binary verification pending the next packaged build (blocked, below) |

## Commands and counts

| Command | Result |
| --- | --- |
| `npm ci` | clean install per `package-lock.json`, no audit errors |
| `npm run check` (`tsc --noEmit`) | exit 0, no diagnostics |
| `npm test` (`vitest run`) | **18 test files, 594/594 passed, 0 failed** (~16 s) |
| `npm run build` (`tsc --noEmit && vite build`) | succeeded — `dist/index.html` 47.23 kB, `index-D5Jyx_jw.js` 137.08 kB (gzip 40.75 kB), `index-B454LGtB.css` 27.92 kB |
| `cargo test -p vault-core` | **44 test-result lines (lib unittests + 42 integration binaries + doc-tests), 790 passed / 0 failed / 0 ignored**, with the toolchain substitution described above |

Count drift is expected and is not a regression signal: the frontend suite was 554/554 at #89's merge time and 563/563 during planning on `ffb9229`; the 594 here adds the suites from #119 and #122–#131 that landed in between.

## Blocked verification (not run, with reasons)

| Check | Blocker |
| --- | --- |
| e2e suite (`npm run test:e2e`, incl. #88's new `16-workspace-layout` spec) | Needs the Tauri/WebKitGTK app and a display; this runner has no WebKitGTK system libraries and no Tauri runtime (runner limit, not a code failure) |
| Packaged `.deb`/AppImage build + smoke test (docs/release.md route) | Bundling needs the webkit2gtk dev stack and packaging tools; not installable without sudo. #87's binary phantom-scroll check and #89's packaged smoke stay pending the next Docker-based build |
| Docker suite image (docs/testing.md) | No Docker daemon on this runner |
| `cargo test --workspace` (`src-tauri` member) | `src-tauri` cannot compile without webkit2gtk/javascriptcoregtk dev libraries — the same host limitation recorded in the [retrieval slice-1 evidence](retrieval-slice-1-results-2026-11-14.md); container route per docs/release.md |
| macOS `.dmg` smoke | No macOS environment |
| GitHub Actions CI itself | Billing/spending-limit blocked since 2026-11-14 (#102, rel. #80); restoring it is a human account decision, out of scope here |

## What remains

The blocked list above is the open remainder of #102's third bullet (packaged-app verification for the UI wave) plus the full-suite rerun items that need Docker or macOS. Clearing them requires restored Actions billing or a Docker-capable environment — a decision tracked on #102, not something this record can do. No application code changed in this verification pass (`git diff` touches only this file).
