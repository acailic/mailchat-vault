# Contributing to Mailchat Vault

## License and contribution terms

The project is licensed under **GPL-3.0-only** ([LICENSE](LICENSE); the decision and its rationale are [D13](docs/decisions.md)). By contributing, you agree that your contribution is licensed under GPL-3.0-only with no additional terms.

We use the **Developer Certificate of Origin (DCO)**: every commit must include a `Signed-off-by: Your Name <you@example.com>` line (`git commit -s` adds it). The DCO is your statement that you have the right to submit the contribution under this license — its text is at <https://developercertificate.org/>.

There is **no CLA, and no relicensing grant**. Contributions remain under GPL-3.0-only; the project will neither ask for nor informally accept any right to relicense your contribution. If relicensing were ever considered, it would be a separate, reviewed process announced in advance — see D13.

Documentation, logos, icons and screenshots may carry licensing of their own; check the file or its source before reusing them in other projects.

## Scope

Mailchat Vault is a personal, local email and chat archive ([D01](docs/decisions.md)); unified-inbox, collaboration and hosted-service features are deliberate non-goals. Read the [product plan](docs/product-plan.md) before proposing features, and open an issue first for anything beyond a small fix so the approach can be agreed. Work lands in small, reviewable slices: one user problem, existing-milestone dependency, acceptance behavior and evidence path (see the [documentation index](docs/README.md) for the working conventions).

## Development

Setup, prerequisites and the test suites are in the [README](README.md): `npm test`, `npm run check`, `cargo test` (plus `--workspace` where the Tauri prerequisites exist). CI runs the suites with `clippy` and `fmt --check` — but while Actions is down for billing, the operative per-PR gate is the [interim local verification protocol](docs/testing.md#interim-per-pr-verification-while-ci-is-down), recorded in the PR body.

Update the affected documentation in the same change as a behavior change (see [docs maintenance](docs/README.md#maintenance)); use synthetic fixtures and never commit real message content or credentials.

## Security issues

Do not open public issues for security problems — follow [SECURITY.md](SECURITY.md).
