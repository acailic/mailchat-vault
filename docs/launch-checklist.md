# Launch checklist — owner actions

Supply-chain automation that can be committed from the repository landed
with P3·19 ([`deny.toml`](../deny.toml), the supply-chain workflow
(staged at [`supply-chain.workflow.yml`](supply-chain.workflow.yml) —
see its item below), the release SBOM convention in [release.md](release.md)). What follows is
the part that **cannot** be done from a commit: every item below is an
account/repository-owner action. Each lists why it matters and the state
it retires. This page is the handoff: when an item is done, check it off
and update the cross-linked status.

- [ ] **Restore GitHub Actions billing** (account Settings → Billing &
  plans: fix the failed payment / raise the spending limit).
  Why: every workflow — CI, release, and the new supply-chain jobs —
  starts no runners since 2026-11-14; per D11, workflow YAML is not
  evidence, so the deny/audit/SBOM jobs committed in P3·19 remain
  unexecuted config until a real run exists.
  Retires: the "pending — retired when Actions billing is restored"
  legs recorded in every recent PR ([#80](https://github.com/acailic/mailchat-vault/issues/80),
  [#102](https://github.com/acailic/mailchat-vault/issues/102)).
  Verify: any workflow run gets past job start (no payments annotation).

- [ ] **Make the repository public, then enable OpenSSF Scorecard
  analysis and uncomment the README badge.**
  Why: the go-public review expects a supply-chain score; Scorecard's
  scheduled analysis only runs on public repos, so the (committed,
  commented-out) badge line in [`README.md`](../README.md) would render
  an empty score while private. The visibility move itself waits on the
  rights audit ([#79](https://github.com/acailic/mailchat-vault/issues/79)) —
  do not flip visibility for the badge alone.
  Retires: the commented-out badge placeholder and this checklist line.
  Verify: the badge renders a score at the README top and the
  Scorecard action has a scheduled run.

- [ ] **Enable Dependabot** (Settings → Code security → Dependabot:
  version updates + alerts, Cargo `Cargo.toml` + npm `package.json`).
  Why: cargo-deny/audit gate what is already in the lock at CI time;
  Dependabot is what proposes the upgraded lock before advisories land
  in a release. The advisory waivers recorded in `deny.toml`'s HIT
  RECORD are designed to be retired by these updates.
  Retires: the "dedup/advisory churn belongs to dependency-upgrade
  tasks" debt noted in [`deny.toml`](../deny.toml).
  Verify: Dependabot is active in Settings and update PRs/alerts arrive;
  no `.github/dependabot.yml` is committed by P3·19 (enabling in
  Settings owns the config).

- [ ] **Configure branch protection on `main`** (Settings → Branches:
  require PR reviews, require conversation resolution, restrict
  force-push/deletion).
  Why: the interim per-PR protocol
  ([testing.md](testing.md#interim-per-pr-verification-while-ci-is-down))
  assumes PRs are the only route to `main`; while Actions is down the
  local verification record in the PR body is the gate, and branch
  protection is what makes that record non-bypassable. Also a scored
  factor once Scorecard runs.
  Retires: the "branch protection = owner action" line recorded in the
  P3·19 PR body.
  Verify: a direct push to `main` is rejected.

- [ ] **Move the staged supply-chain workflow into place.**
  Why: P3·19's deny/audit/SBOM jobs are complete and validated as YAML,
  but are committed at [`docs/supply-chain.workflow.yml`](supply-chain.workflow.yml)
  — the automation token that produced them lacks the GitHub
  `workflows` permission (account-level; pushes touching
  `.github/workflows/` are refused), which no repository change can
  grant.
  Retires: this line and the staged file. One command:
  `git mv docs/supply-chain.workflow.yml .github/workflows/supply-chain.yml`.
  The jobs still start no runners until the billing item above is also
  done.

- [ ] **Adopt signed tags and signed releases** (create release tags with
  a GPG/SSH-signed `git tag -s`/`-s` equivalent; long term, commit
  signing too).
  Why: `SHA256SUMS` is the integrity baseline for release assets
  ([release.md](release.md#checksums--verifying-a-published-release)),
  but it authenticates bytes, not authorship — a signed tag is the
  "these are my bytes" statement the checksum file rides on. The
  v0.2.0 tag (`71ac2d4`) is unsigned; that is acceptable while the repo
  is private, and is a public-launch expectation to fix.
  Retires: the "signed tags = owner action" line recorded in the P3·19
  PR body; also improves the future Scorecard token-permissions posture.
  Verify: `git tag -v v<next>` verifies against a published key.

Related, deliberately separate: artifact attestations have their own
preconditions (executing workflows + eligible plan) recorded in
[release.md → Provenance](release.md#provenance-github-artifact-attestations-are-a-separate-plan-with-preconditions);
code signing/notarization has its own plan in
[release.md → Code-signing status](release.md#code-signing-status-this-round-none).
