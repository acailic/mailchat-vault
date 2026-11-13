# Mailchat Vault

Local email & chat archive — a Tauri 2 desktop app (Rust backend, vanilla TypeScript frontend) with SQLite FTS5 full-text search over your own messages. A private tool: nothing leaves the machine, except what you explicitly send for optional [AI-assisted search](docs/ai-search.md) — your question and the top matching rows only, behind an explicit consent gate, and only when an API key is configured.

Import `.mbox` files and WhatsApp / Telegram / Signal / Discord exports, sync mail incrementally over IMAP (Gmail via Google sign-in or app password), then search it all with filters, conversation context, and export. Desktop Linux, macOS, and Windows.

## Highlights

- **Import** — mbox files and WhatsApp / Telegram / Signal / Discord exports (single files, folders, or `.zip`), each with a bounded preview before anything is stored; referenced media is extracted into a content-addressed store.
- **IMAP sync** — incremental, UIDVALIDITY-tracked sync from Gmail (Google sign-in / OAuth2 or app password) or any IMAP server; opt-in scheduled auto-sync, folder watching, and new-mail desktop notifications.
- **Search** — FTS5 full-text with match highlighting, filters (source, author, date, attachments), pagination, and a reading pane with chronological conversation context and keyboard navigation.
- **Optional AI** — natural-language questions over the archive with source-linked citations, ranked hybrid (FTS5 + local embeddings); consent-gated and privacy-scoped, and fully functional offline without a key.
- **Safety** — idempotent, deduplicating ingest (re-import and re-sync never duplicate), safe migrations with snapshot and rollback, staged backup/restore, and a background-jobs pool that keeps search responsive during imports and syncs.
- **Cross-platform** — Linux (AppImage/deb), macOS (dmg), Windows (msi/exe).

Full detail: [feature reference](docs/features.md).

## Documentation

| Guide | Contents |
| --- | --- |
| [Feature reference](docs/features.md) | Every feature in detail: sources and import, workspace, folder watching, background jobs, export, media lifecycle, chat-format notes |
| [AI-assisted search](docs/ai-search.md) | Ask pipeline, AI consent, provider configuration (Settings and environment), local embeddings and hybrid ranking |
| [IMAP sync](docs/imap-sync.md) | Gmail (Google sign-in / app password) and generic IMAP setup, credential storage |
| [Testing](docs/testing.md) | Test suites, performance budgets, fuzzing, CI |
| [Specification set](docs/README.md) | Product plan, UX specification, architecture, data and API contracts, roadmap, decisions |
| [Releases](docs/release.md) | Installers, smoke tests, checksums, publishing |

## Quickstart

Prerequisites: [Node.js](https://nodejs.org) 22+, [Rust](https://rustup.rs) stable, and on Ubuntu/Debian the Tauri system libraries (macOS/Windows need Xcode / MSVC as usual):

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

```sh
npm install
npm run tauri dev
```

An empty archive opens on **Add your first source** — import any `.mbox` or chat export (a bounded preview shows before anything is stored) or explore the isolated sample archive. The vault database lives in your app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux) with extracted media under `media/` next to it. The full walkthrough: [first-run tour](docs/features.md#first-run-tour).

## Repository layout

```
crates/vault-core/   # storage, search, ingest, IMAP sync, jobs, media, backup — no UI deps
src-tauri/           # Tauri app shell: commands, events, LLM provider registry, keyring secrets
src/                 # frontend (vanilla TS): workspace shell, search, sources, settings
```

The annotated layout (module by module) is in the [feature reference](docs/features.md#annotated-repository-layout).

## Development

```sh
npm test                 # frontend unit + UI-state snapshot tests
npm run check            # TypeScript type-check
cargo test               # vault-core suite
cargo test --workspace   # also the Tauri app tests
```

Performance budgets, fuzzing, and the hermetic IMAP/OAuth test setup: [docs/testing.md](docs/testing.md). CI runs all of it plus `clippy` and `fmt --check` on every push and PR.

## Releases

Pushing a version tag (`v*`) builds Linux, macOS, and Windows installers as a **draft** GitHub Release with a `SHA256SUMS` checksum asset; artifacts are unsigned and smoke-tested before publishing. The local build/publish route — currently the operative path — and every gate: [docs/release.md](docs/release.md).
