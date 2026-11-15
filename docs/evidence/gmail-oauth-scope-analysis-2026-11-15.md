# Gmail OAuth scope analysis — keep BYO-client as advanced setup (2026-11-15)

Status: **analysis and recommendation recorded** against [#82](https://github.com/acailic/mailchat-vault/issues/82). The launch decision authority is the working go-public plan (kept outside this repository per the 2026-11-14 decision, [docs index](../README.md)); this record is its in-repo evidence surface. Sources were fetched and read 2026-11-15.

## Question

Can `oauth_gmail_login` request less than the full mail scope (least privilege), and what would shipping a public OAuth client require? ([#82](https://github.com/acailic/mailchat-vault/issues/82), go-public review finding S7.)

## Findings

### 1. Gmail XOAUTH2 requires the full restricted scope — read-only commands do not reduce it

Google's XOAUTH2 documentation ([S7](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol#oauth_20_scopes), reviewed 2026-11-15) states:

> The scope for IMAP, POP, and SMTP access is `https://mail.google.com/`.
> - To be approved, your app must show full utilization of `https://mail.google.com/`.
> - If your app does not require `https://mail.google.com/`, migrate to the Gmail API and use more granular restricted scopes.

The SASL error example in the same document shows the refusal shape: a token whose scope does not match gets a 401 challenge whose `scope` field names `https://mail.google.com/` back — i.e. the scope is checked at `AUTHENTICATE`, before any command runs.

The vault's IMAP usage is already strictly read-only (`EXAMINE`, `UID SEARCH`, `UID FETCH` — [imap.rs](../../crates/vault-core/src/imap.rs), asserted by the hermetic suites). But the OAuth *grant*, not the command set, defines the permission: the consent grants read-write mail access either way, and Gmail's endpoint refuses narrower tokens outright.

### 2. No scope-reduction code change is warranted on the IMAP path

Changing [`GMAIL_SCOPE`](../../crates/vault-core/src/oauth.rs) to a narrower URL (e.g. `https://www.googleapis.com/auth/gmail.readonly`) would break every Google sign-in at SASL auth. The constant is now pinned by a unit test with the rationale, so a well-meant least-privilege patch cannot silently break login. This is the deliberate "no code change" outcome of the analysis, not an omission.

### 3. The only real least-privilege path is a Gmail API connector — a separate project

`https://www.googleapis.com/auth/gmail.readonly` is a granular *Gmail API* scope. Using it means a second sync engine (REST `messages.list`/`messages.get` pagination instead of the UIDVALIDITY/UID cursor model, different batching, failure taxonomy, and media parity) — not a scope tweak. It is the verification-friendly public path (see finding 4), and by that fact a separate project ([#82](https://github.com/acailic/mailchat-vault/issues/82)).

### 4. A verified public client with the full mail scope is doubly blocked for this app

- **Restricted-scope verification:** `https://mail.google.com/` is a restricted scope. A public client (credentials shipped in the app) must complete OAuth verification, which for restricted scopes includes a third-party security assessment (CASA tier 2 or above via an authorized lab; annual recertification), consent-screen branding, a privacy policy and home page, and a demo video — multi-week lead time at best, with lab cost. ([Verification requirements](https://support.google.com/cloud/answer/13464321), [security assessment](https://support.google.com/cloud/answer/13465431), [restricted scopes](https://support.google.com/cloud/answer/13464325).)
- **Full-utilization requirement:** S7 says approval requires showing *full utilization* of the full mail scope. An archive client whose IMAP session is deliberately `EXAMINE`-only cannot show full utilization; Google's own guidance directs such apps off IMAP to the Gmail API. A read-only IMAP client is therefore a weak verification case even after paying the assessment.

By contrast, the current BYO-client design needs none of this: Google exempts **personal-use apps with fewer than 100 users** from verification (users click through the "unverified app" screen on their own project), and each user consents on a client they own ([when verification is not needed](https://support.google.com/cloud/answer/13464323)).

## Decision (recorded; product-owner sign-off tracked in #82)

1. **Keep Google sign-in as the labeled advanced, BYO-client setup** — status quo, no new code path advertised.
2. **Do not advertise one-click public Gmail access.** Reviewed README, `index.html`, the first-run/Add-source UI strings, and `docs/imap-sync.md`: none promises public/one-click Gmail; the unconfigured state honestly names the two env vars (product-review F04 already records this as intended).
3. **A Gmail-API read-only connector or a verified public client is a separate project**, to be scoped only if consumer Gmail onboarding becomes a goal. Not a prerequisite for the file-import pilot.
4. The scope constant stays the full mail scope, pinned by test (finding 2).

## USER steps if a public client is ever pursued (outside this repository)

These are Google-console/account actions with external lead time; none can be done from the repo or this runner:

1. Own a Google Cloud project with an OAuth client for the app; configure the consent screen (app name, logo, home page, privacy policy URL) and set publishing status to *In production*.
2. Submit the OAuth verification request declaring the restricted scope `https://mail.google.com/` (or build the Gmail-API connector and declare `gmail.readonly` instead — the stronger case).
3. Complete the security assessment (CASA tier 2+ via an authorized lab; self-scan route may qualify at tier 2) and the annual recertification.
4. Provide the demo video and respond to reviewer follow-ups; budget multi-week lead times and possible rejection on the full-utilization grounds above.
5. Repo-side (follow-up task, not this change): ship the client credential path (env fallback → packaged credential or token-broker) and update the consent/honesty copy and tests.

## Evidence trail

| Claim | Source |
| --- | --- |
| IMAP/POP/SMTP scope is exactly `https://mail.google.com/`; full-utilization requirement; migrate-to-Gmail-API guidance | [XOAUTH2 protocol doc, OAuth 2.0 scopes](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol#oauth_20_scopes) (fetched 2026-11-15) |
| Scope checked at SASL auth; error challenge names the required scope | same document, SASL error-response sections |
| Personal-use (<100 users) apps exempt from verification | [When verification is not needed](https://support.google.com/cloud/answer/13464323) (fetched 2026-11-15) |
| Restricted-scope verification includes CASA assessment, annual recertification | [Verification requirements](https://support.google.com/cloud/answer/13464321) · [Security assessment](https://support.google.com/cloud/answer/13465431) · [Annual recertification](https://support.google.com/cloud/answer/13463816) |
| Vault issues only `EXAMINE`, `UID SEARCH`, `UID FETCH` | [imap.rs](../../crates/vault-core/src/imap.rs) sync path; hermetic IMAP suites |
| Scope constant pinned; BYO rationale in code | [oauth.rs](../../crates/vault-core/src/oauth.rs) `GMAIL_SCOPE` + tests |
| No one-click/public Gmail claim in UI or docs | README, `index.html`, `src/main.ts` sign-in hints, `docs/imap-sync.md`, [product-review F04](../product-review.md) |
