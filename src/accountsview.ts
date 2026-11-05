// Pure helpers for the multi-account UI (roadmap: the accounts panel) —
// unit tested, no DOM beyond the renderer below; every dynamic string
// goes through textContent, so labels and errors can never inject markup.
//
// The accounts panel lists every account derived from the vault's durable
// source instances — IMAP accounts (email) and import accounts (mbox/chat
// exports) — with per-account stats, last-sync time, and the management
// actions: sync now (re-runs `sync_imap` for that account, reusing its
// stored credentials), pause/resume (per-source exclusion), and forget
// credentials (removes the keyring secrets, keeps the archive).

import type { AccountView } from "./types";
import type { ImapAuthMode } from "./ipc";

/** The arguments of one `sync_imap` invocation for a stored account. */
export interface SyncNowRequest {
  host: string;
  port: number | null;
  username: string;
  password: string;
  folders: string[] | null;
  auth: ImapAuthMode;
}

/** Derive the `sync_imap` request for one account row: the stored
 * connection identity, an empty password (the backend resolves the stored
 * keyring secret — the coordinator's password-first precedence), and the
 * auth mode the stored credentials allow. `null` for import accounts
 * (nothing to sync) or an IMAP account without an identity. */
export function syncNowRequest(account: AccountView): SyncNowRequest | null {
  if (account.kind !== "imap" || !account.imap) return null;
  const { host, port, username, folders, has_password, has_oauth } = account.imap;
  const oauthOnly = has_oauth && !has_password;
  return {
    host,
    port,
    username,
    password: "",
    folders: folders.length > 0 ? folders : null,
    // The coordinator's precedence: stored password first, then the
    // Google sign-in. An account with nothing stored syncs as "password"
    // so the backend's error tells the user to enter one.
    auth: oauthOnly ? "oauth" : "password",
  };
}

/** One-line kind label for an account row. */
export function accountKindLabel(account: AccountView): string {
  return account.kind === "imap" ? "Email (IMAP)" : "Import";
}

/** The account's headline label: the label itself plus the username for
 * IMAP accounts (the label already carries `imap:host:port:user`, but the
 * username is what the user knows the account by). */
export function accountTitle(account: AccountView): string {
  return account.label;
}

/** One-line per-account sync state: connected/paused/never-synced, the
 * last successful sync, the failed-item backlog, and which credentials
 * are stored. Plain text, screen-reader friendly (the row also carries a
 * live region for action results). */
export function describeAccountSync(account: AccountView): string {
  const parts: string[] = [];
  if (account.kind === "imap") {
    parts.push(
      account.excluded
        ? "paused — auto-sync skips this account (archive kept)"
        : account.state === "connected"
          ? "connected"
          : "not connected yet",
    );
    if (account.last_success_at) {
      parts.push(`last synced ${account.last_success_at}`);
    } else {
      parts.push("never synced");
    }
    if (account.imap) {
      const creds: string[] = [];
      if (account.imap.has_password) creds.push("password");
      if (account.imap.has_oauth) creds.push("Google sign-in");
      parts.push(creds.length > 0 ? `credentials: ${creds.join(" + ")}` : "no credentials stored");
    }
  } else {
    parts.push(account.last_attempt_at ? `imported ${account.last_attempt_at}` : "imported");
  }
  if (account.failed_items > 0) {
    parts.push(`${account.failed_items} failed item${account.failed_items === 1 ? "" : "s"} retrying`);
  }
  return parts.join(" · ");
}

/** The empty-state line when no account exists yet. */
export function accountsEmptyLine(): string {
  return "No accounts yet — connect an email account (IMAP) or import a chat/mbox export and it appears here.";
}

/** The per-account stats fragment: message count (+ failed backlog). */
export function accountStatsLine(account: AccountView): string {
  const messages = `${account.message_count.toLocaleString()} message${account.message_count === 1 ? "" : "s"}`;
  return account.failed_items > 0 ? `${messages} · ${account.failed_items} failing` : messages;
}

export interface RenderAccountsOptions {
  onSyncNow: (account: AccountView) => void;
  onTogglePause: (account: AccountView) => void;
  onForget: (account: AccountView) => void;
}

/** Render the accounts list (replaces children): one row per account —
 * label, kind, stats, sync state — plus the IMAP management actions
 * (sync now / pause-resume / forget credentials). Every control is a
 * real focusable button with an account-scoped accessible name, so the
 * keyboard/screen-reader path stays usable per the workspace
 * conventions; `Esc` keeps its global meaning (drawer, reading pane). */
export function renderAccounts(
  el: HTMLElement,
  accounts: AccountView[],
  opts: RenderAccountsOptions,
): void {
  if (accounts.length === 0) {
    const empty = document.createElement("li");
    empty.className = "account-row empty";
    const line = document.createElement("span");
    line.className = "meta";
    line.textContent = accountsEmptyLine();
    empty.append(line);
    el.replaceChildren(empty);
    return;
  }
  const rows = accounts.map((account) => {
    const li = document.createElement("li");
    li.className = "account-row";
    li.dataset.sourceId = account.source_id;

    const main = document.createElement("div");
    main.className = "account-main";
    const title = document.createElement("span");
    title.className = "account-label";
    title.textContent = accountTitle(account);
    const kind = document.createElement("span");
    kind.className = "account-kind meta";
    kind.textContent = accountKindLabel(account);
    const stats = document.createElement("div");
    stats.className = "account-stats meta";
    stats.textContent = accountStatsLine(account);
    const sync = document.createElement("div");
    sync.className = "account-sync meta";
    sync.textContent = describeAccountSync(account);
    main.append(title, kind, stats, sync);

    li.append(main);

    if (account.kind === "imap") {
      const actions = document.createElement("div");
      actions.className = "account-actions";

      const syncNow = document.createElement("button");
      syncNow.type = "button";
      syncNow.className = "account-sync-btn";
      syncNow.textContent = "Sync now";
      syncNow.title = `Sync ${account.label} now, reusing its stored credentials`;
      syncNow.setAttribute("aria-label", `Sync account ${account.label} now`);
      syncNow.addEventListener("click", () => opts.onSyncNow(account));

      const pause = document.createElement("button");
      pause.type = "button";
      pause.className = "account-pause-btn";
      pause.textContent = account.excluded ? "Resume" : "Pause";
      pause.title = account.excluded
        ? `Include ${account.label} in auto-sync rounds again`
        : `Skip ${account.label} in auto-sync rounds (archive and cursors kept)`;
      pause.setAttribute(
        "aria-label",
        `${account.excluded ? "Resume" : "Pause"} account ${account.label}`,
      );
      pause.addEventListener("click", () => opts.onTogglePause(account));

      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "account-forget-btn";
      forget.textContent = "Forget credentials";
      forget.title = `Remove ${account.label}'s stored password and Google sign-in from the OS keyring (the archive stays)`;
      forget.setAttribute("aria-label", `Forget stored credentials of account ${account.label}`);
      forget.addEventListener("click", () => opts.onForget(account));

      actions.append(syncNow, pause, forget);
      li.append(actions);
    }

    return li;
  });
  el.replaceChildren(...rows);
}
