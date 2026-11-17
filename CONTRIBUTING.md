# Contributing to Mailchat Vault

## Quick path

Prerequisites: [Node.js](https://nodejs.org) 22+, [Rust](https://rustup.rs) stable, and on Ubuntu/Debian the Tauri system libraries (`sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`; macOS/Windows need Xcode / MSVC).

```sh
git clone https://github.com/acailic/mailchat-vault.git
cd mailchat-vault
npm install
npm test          # frontend unit + UI-state tests
npm run check     # TypeScript type-check
cargo test        # vault-core suite
```

Any one of the three suites turning green is a healthy first signal from a fresh clone. For orientation, start with [docs/architecture.md](docs/architecture.md) (components, data flows, privacy boundaries); the full documentation index is [docs/README.md](docs/README.md).

PR expectations: small, reviewable slices — one user problem per PR, with acceptance behavior and its evidence path stated in the description. Sign every commit with the DCO (`git commit -s`). While GitHub Actions is down for billing, run the [interim per-PR verification protocol](docs/testing.md#interim-per-pr-verification-while-ci-is-down) locally and record the result in the PR body.

## License and contribution terms

The project is licensed under **GPL-3.0-only** ([LICENSE](LICENSE); the decision and its rationale are [D13](docs/decisions.md)). By contributing, you agree that your contribution is licensed under GPL-3.0-only with no additional terms.

We use the **Developer Certificate of Origin (DCO)**: every commit must include a `Signed-off-by: Your Name <you@example.com>` line (`git commit -s` adds it). The DCO is your statement that you have the right to submit the contribution under this license — its text is at <https://developercertificate.org/>.

There is **no CLA, and no relicensing grant**. Contributions remain under GPL-3.0-only; the project will neither ask for nor informally accept any right to relicense your contribution. If relicensing were ever considered, it would be a separate, reviewed process announced in advance — see D13.

Documentation, logos, icons and screenshots may carry licensing of their own; check the file or its source before reusing them in other projects.

## Scope

Mailchat Vault is a personal, local email and chat archive ([D01](docs/decisions.md)); unified-inbox, collaboration and hosted-service features are deliberate non-goals. Read the [product plan](docs/product-plan.md) before proposing features, and open an issue first for anything beyond a small fix so the approach can be agreed. Work lands in small, reviewable slices: one user problem, existing-milestone dependency, acceptance behavior and evidence path (see the [documentation index](docs/README.md) for the working conventions).

## Development

The suites and prerequisites are listed in the [Quick path](#quick-path) above. CI runs them with `clippy` and `fmt --check` — but while Actions is down for billing, the operative per-PR gate is the [interim local verification protocol](docs/testing.md#interim-per-pr-verification-while-ci-is-down), recorded in the PR body.

Update the affected documentation in the same change as a behavior change (see [docs maintenance](docs/README.md#maintenance)); use synthetic fixtures and never commit real message content or credentials.

## Security issues

Do not open public issues for security problems — follow [SECURITY.md](SECURITY.md).
