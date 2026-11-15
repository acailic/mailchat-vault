# IMAP sync (Gmail & friends)

The **IMAP sync** section pulls mail straight from a server into the archive — incrementally: each folder's `UIDVALIDITY` + highest-seen UID are tracked in the vault database (keyed by the account's durable source instance), so every sync fetches only new messages. If a folder is recreated (`UIDVALIDITY` changes), it is rescanned and re-fetched messages are deduped through the connector identity — a reset never creates duplicate rows. The folder name is stored as the message `channel`.

Each synced account is a **durable source instance**: its identity is the canonical connector configuration — case-folded host, port, TLS mode, and the exact username — while the password stays only in the OS keyring (or process memory). Reconfiguring credentials preserves the instance; two accounts on different hosts stay distinct even with the same username; and the same mail seen in several folders is one archived message with one folder occurrence per sighting.

## Coordinator rounds, atomic batches, failed-item accounting (M2.3)

The **`imap_sync_all`** background job runs one coordinator round over
every configured account at once (the single-account **Sync** button keeps
its explicit per-account form). A round:

- **skips excluded sources** — the per-source exclusion flag
  (`sources_list` / `source_set_excluded`) pauses an account's syncing
  while keeping its archive, cursors, and configuration untouched; an
  explicit single-account sync still runs for an excluded source
  (manual override stays manual);
- **resolves credentials per source** from the OS keyring — a stored
  password, else a stored Google sign-in refreshed into a fresh access
  token; an account with no stored credentials is **reported**
  (`no_credentials`), not errored;
- **never lets one source sink the round** — an unreachable or failing
  account carries its error in the round summary while the others sync.

Every sync — manual or coordinator — commits work in **atomic fetch
batches**: one `UID FETCH` batch's messages, its failed-item ledger rows,
and the checkpoint advance land in a single transaction, so an
interruption (dead connection, killed process) always leaves the cursor
exactly at the boundary of what is durably stored; the next round
resumes from there without duplicates. Items the round accounts but
cannot store are **ledgered durably** — an unparseable mail, or a uid the
server searched but never delivered (typically expunged between the two
commands) — with a fixed reason and an attempt count, visible through
**`source_failures`** (what fell, where, why, how often retried), and
re-fetched by every later round until they resolve. A `UIDVALIDITY`
reset rescans the folder from UID 1, dedups through the connector
identity, and clears the abandoned validity's ledger rows in the same
atomic commit that adopts the new validity.

## Authentication

Two ways to authenticate, picked in the UI:

### Gmail (Google sign-in / OAuth)

**Advanced setup** — this path needs a Google Cloud project you register yourself (below); the app deliberately ships no public one-click Gmail sign-in, and the full-mail scope below cannot be narrowed while staying on IMAP ([scope analysis](evidence/gmail-oauth-scope-2026-11-15.md), issue #82). The simpler default is the [app password](#gmail-app-password) path.

The browser-based desktop OAuth2 flow: click **Sign in with Google…**, consent once in the browser window that opens, done — the app receives an access token plus a refresh token and authenticates IMAP via SASL `XOAUTH2`. No app password needed, and every sync refreshes a fresh short-lived access token from the stored refresh token.

**You register the OAuth client yourself** (one-time, free):

1. In [Google Cloud Console](https://console.cloud.google.com/apis/credentials) create an **OAuth client ID** of type **Desktop app** (any project works; no publishing needed for personal use).
2. Add `http://127.0.0.1` as an **Authorized redirect URI** (the app redirects the browser to `http://127.0.0.1:<random port>` — the port is ignored for loopback URIs).
3. Copy the **client ID** and **client secret** into the environment before starting the app:

   | Variable | Meaning |
   | --- | --- |
   | `MAILCHAT_GMAIL_CLIENT_ID` | your OAuth client ID (`…apps.googleusercontent.com`) |
   | `MAILCHAT_GMAIL_CLIENT_SECRET` | your client secret (`GOCSPX-…`) |

   Both must be set (or both unset — then the Google sign-in UI says it is not configured). The scope requested is `https://mail.google.com/` — the full, restricted mail scope Gmail's IMAP endpoint demands for `AUTHENTICATE XOAUTH2`. The app itself only ever issues read-only IMAP commands (`EXAMINE`, `UID SEARCH`, `UID FETCH`), but that does **not** narrow the OAuth permission: Google refuses narrower tokens at IMAP login and directs apps that do not need the full scope to the Gmail API instead ([scope analysis](evidence/gmail-oauth-scope-2026-11-15.md)). Personal-use clients like this one (your own, under 100 users) are exempt from Google's verification.
4. In the app: choose **Google sign-in (OAuth)**, enter your address, sign in once, then **Sync** like with any other account.

### Gmail (app password)

1. Enable 2-step verification on the Google account, then create an **App password** (Google Account → Security → 2-Step Verification → App passwords).
2. In the app: host `imap.gmail.com`, port `993`, username `you@gmail.com`, the app password, folders e.g. `INBOX` (comma-separated; empty means `INBOX`).
3. Click **Sync**. The first sync fetches the whole folder; later syncs only new mail.

### Other servers

Any IMAP server with implicit TLS works the same way via the app-password path: host, port (993), username, password, folders.

## Where credentials live

- **Typed passwords** are used for the sync and then stored in the **OS keyring** when one is available (Secret Service/GNOME Keyring or KWallet on Linux, Keychain on macOS, Credential Manager on Windows) — service `mailchat-vault`, entry `user@host`.
- **The OAuth refresh token** from a Google sign-in lives in the same OS keyring under the entry `oauth:user@host`. Short-lived access tokens exist only in process memory for the duration of a sync.
- With no OS keyring reachable, the password or refresh token is kept **only in process memory for the current app session** (gone on exit) — the UI says so.
- Secrets are never written to the vault database (tests byte-scan the DB files for the password *and* the refresh token to enforce this) and never logged. `ImapConfig`'s `Debug` output is redacted too.
- Leaving the password field empty reuses the stored password; to remove it entirely, delete the entry in your OS keyring manager. To redo a Google sign-in, sign in again (it replaces the stored refresh token).
