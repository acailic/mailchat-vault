# Security policy

## Supported versions

The latest tagged release. Release artifacts are currently **unsigned**; verify downloads against the published `SHA256SUMS` (see [release documentation](docs/release.md) and the [support matrix](docs/support-matrix.md)).

## Reporting a vulnerability

Use GitHub's private vulnerability reporting — **"Report a vulnerability"** on this repository's **Security** tab — or a security advisory. Do not open a public issue for a suspected vulnerability.

Include the app version, OS, and reproduction steps. **Do not include real message content, credentials, keys or archive data** in a report unless specifically asked — synthetic fixtures reproduce almost everything (see [testing](docs/testing.md) for the fixture approach).

## Scope

In scope: the desktop application, its release pipeline and packaging scripts. Out of scope: the app not encrypting the vault or backups at rest — that is a documented, deliberate limitation ([D07](docs/decisions.md): OS account/disk protection covers data at rest), not a vulnerability.

## Response

Best-effort; a two-business-day initial triage target is being matched to actual capacity before publication ([go-public plan §8](docs/go-public-plan.md)). Reports of data loss, corruption or unintended content transmission pause all expansion work immediately.
