// Pure helpers for the sources UI (roadmap: the Sources list) —
// unit tested, no DOM beyond the renderer below; every dynamic string
// goes through textContent, so labels and errors can never inject markup.
//
// The Sources list shows every account derived from the vault's durable
// source instances — IMAP accounts (email) and import accounts (mbox/chat
// exports) — with per-source stats and last-sync time. View messages (and
// Sync now for email) are the primary actions; the management actions —
// pause/resume (per-source exclusion), update credentials, forget
// credentials (removes the keyring secrets, keeps the archive), and the
// two-step disconnect — live behind each email row's Details fold.

import type { AccountView, SourceCount, SourceInstance } from "./types";
import type { ImapAuthMode } from "./ipc";
import { friendlySourceLabel } from "./format";
import { iconEl, type IconName } from "./icons";

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

/** The taxonomy icon of one source: live email → the envelope, a chat
 * export (WhatsApp/Telegram/Signal/Discord/Messenger — the backend's
 * `"chat"` medium) → the chat bubble, everything else (mbox/.eml
 * imports, mixed or not-yet-archived imports) → the file sheet. The
 * medium comes from the messages' own `channel` column, so the icon
 * never guesses from the label. */
export function sourceIcon(account: Pick<AccountView, "kind" | "medium">): IconName {
  if (account.kind === "imap") return "mail";
  return account.medium === "chat" ? "chat" : "file";
}

/** The account's headline label — the identity the user knows the
 *  account by, never the exposed implementation token. An IMAP account
 *  is known by its address and server (`ana@gmail.com · imap.gmail.com`),
 *  not by the stored `imap:host:port:user` connector label; an import
 *  keeps its label in plain words (`friendlySourceLabel` rewrites the
 *  raw `mbox` namespace to "Mail import"). */
export function accountTitle(account: AccountView): string {
  if (account.kind === "imap" && account.imap) {
    return `${account.imap.username} · ${account.imap.host}`;
  }
  return friendlySourceLabel(account.label);
}

/** The tooltip of the account title: the full stored label (the
 * stable source identity) for anyone who needs it — support, bug
 * reports — without making the raw connector key the name everyone
 * reads. Empty when the title already is the stored label. */
export function accountTitleDetail(account: AccountView): string {
  return accountTitle(account) === account.label
    ? ""
    : `Stored source: ${account.label}`;
}

/** One-line per-account sync state: connected/paused/failed/never-synced,
 * the last successful sync, the failed-item backlog, and which credentials
 * are stored. Plain text, screen-reader friendly (the row also carries a
 * live region for action results). A recorded failure (which survives
 * restarts — the account-level error state) leads with what fell and
 * why, in the user's terms. */
export function describeAccountSync(account: AccountView): string {
  const parts: string[] = [];
  if (account.kind === "imap") {
    if (account.excluded) {
      parts.push("paused — auto-sync skips this account (archive kept)");
    } else if (account.last_error) {
      // The last finished attempt failed: say so, with why and when.
      // The configured host/port/username stay on the row — nothing is
      // lost, only the connection fell.
      const at = account.last_error_at ? ` at ${account.last_error_at}` : "";
      parts.push(`sync failed${at} — ${account.last_error}`);
    } else if (account.state === "connected") {
      parts.push("connected");
    } else {
      parts.push("not connected yet");
    }
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

/** Whether a recorded sync failure is about the credentials themselves
 * (none stored, a rejected login, an unusable Google sign-in) — the
 * chip then asks for sign-in instead of quoting the failure. */
function isCredentialFailure(message: string): boolean {
  const m = message.toLowerCase();
  return (
    m.includes("password") ||
    m.includes("credential") ||
    m.includes("sign in") ||
    m.includes("sign-in") ||
    m.includes("login") ||
    m.includes("log in") ||
    m.includes("oauth")
  );
}

/** The compact per-source status for the navigation chip: the same
 * inputs as `describeAccountSync` above, a shorter line, plus the live
 * `syncing` flag. Precedence: a running sync, then sign-in trouble (no
 * credentials stored, or the last failure was about them), then any
 * other recorded failure, then paused, then the last-success line.
 * Imports carry their import time — nothing of theirs syncs. */
export function sourceChipStatus(account: AccountView, syncing: boolean): string {
  if (account.kind !== "imap") {
    return account.last_attempt_at ? `Imported ${account.last_attempt_at}` : "Imported";
  }
  if (syncing) return "Syncing…";
  const noCredentials =
    account.imap !== null && !account.imap.has_password && !account.imap.has_oauth;
  if (noCredentials || (account.last_error !== null && isCredentialFailure(account.last_error))) {
    return "Needs sign-in";
  }
  if (account.last_error) return `Sync failed — ${account.last_error}`;
  if (account.excluded) return "Paused";
  return account.last_success_at ? `Last synced ${account.last_success_at}` : "Never synced";
}

/** The empty-state line when no source exists yet. */
export function sourcesEmptyLine(): string {
  return "No sources yet — import an export or connect an email account and it appears here.";
}

/** Where one just-saved password lives, as the user must be told it:
 * the OS keyring persists across restarts; session memory is cleared
 * when the app exits — the honest-credential rule (a failed sync that
 * already saved the password still must say where it went). */
export function passwordStoreNote(
  savedTo: "os_keyring" | "memory" | null | undefined,
): string {
  if (savedTo === "memory") {
    return "Password kept in memory only for this session (no OS keyring available) — restarting the app clears it.";
  }
  if (savedTo === "os_keyring") {
    return "Password stored in the OS keyring.";
  }
  return "";
}

/** Normalize a `sync_imap` rejection to display text: the classified
 * message when the structured failure shape arrives, the string itself
 * for plain-string rejections (older paths), and — either way — the
 * password-store note when the attempt already saved a password. */
export function syncFailureText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    const failure = e as { message: unknown; saved_to?: unknown };
    const message =
      typeof failure.message === "string" && failure.message.length > 0
        ? failure.message
        : "the sync could not be completed";
    const note = passwordStoreNote(
      failure.saved_to === "os_keyring" || failure.saved_to === "memory" ? failure.saved_to : null,
    );
    return note ? `${message} ${note}` : message;
  }
  return String(e);
}

/** The per-account stats fragment: message count (+ failed backlog). */
export function accountStatsLine(account: AccountView): string {
  const messages = `${account.message_count.toLocaleString()} message${account.message_count === 1 ? "" : "s"}`;
  return account.failed_items > 0 ? `${messages} · ${account.failed_items} failing` : messages;
}

export interface SourceChipRow {
  /** Stable source-instance id (selection key). */
  id: string;
  /** Disambiguated, user-facing label. */
  label: string;
  /** Message count when it belongs to this row alone; null when the
   * label is shared and a per-row count would be a lie. */
  count: number | null;
  /** The source's taxonomy icon (mail / chat / file). Decorative —
   * the label carries the meaning for assistive tech. */
  icon: IconName;
  /** Short per-source status ("Syncing…", "Needs sign-in", "Paused",
   * "Last synced …", "Imported …"); empty when no account row backs
   * the chip — a missing row must not guess a status. */
  status: string;
}

/** Build the navigation chip rows from the canonical instances, the
 * account rows (identity detail), and the label-keyed counts. Selection
 * is by stable id; two sources that share a display label stay
 * independent and get distinct labels. Only sources whose label is
 * present in the counts (≥1 message) get a chip: an empty archive or a
 * not-yet-loaded stats keeps the chip bar empty. `syncing` carries the
 * source ids with a sync in flight, so live rows read "Syncing…". */
export function sourceChipRows(
  instances: SourceInstance[],
  accounts: AccountView[],
  counts: SourceCount[],
  syncing: ReadonlySet<string> = new Set(),
): SourceChipRow[] {
  const accountById = new Map(accounts.map((a) => [a.source_id, a]));
  const countByLabel = new Map(counts.map((c) => [c.source, c.count]));
  const present = instances.filter((s) => countByLabel.has(s.label));
  const labelCounts = new Map<string, number>();
  for (const s of present) {
    labelCounts.set(s.label, (labelCounts.get(s.label) ?? 0) + 1);
  }
  const importSeen = new Map<string, number>();
  const imapSeen = new Map<string, number>();
  /** The chip's taxonomy icon: the account row's medium when the cache
   * has one (mail/chat/file per `sourceIcon`), else the generic file
   * glyph — a missing account row must not lose the chip. */
  function iconOf(id: string): IconName {
    const account = accountById.get(id);
    return account ? sourceIcon(account) : "file";
  }
  /** The chip's status line: derived from the account row (plus the
   * live syncing set); a missing row renders no status at all. */
  function statusOf(id: string): string {
    const account = accountById.get(id);
    return account ? sourceChipStatus(account, syncing.has(id)) : "";
  }
  return present.map((s) => {
    const duplicated = (labelCounts.get(s.label) ?? 0) > 1;
    if (!duplicated) {
      return {
        id: s.id,
        label: friendlySourceLabel(s.label),
        count: countByLabel.get(s.label) ?? null,
        icon: iconOf(s.id),
        status: statusOf(s.id),
      };
    }
    // Email vs import is the instance's own kind; the account row only
    // supplies the imap identity (username · host) detail. The identity
    // disambiguates most duplicate labels, but TLS twins share it too
    // (same server reachable over two modes): number within the
    // identity, import-style, so every chip label stays distinct.
    const account = accountById.get(s.id);
    let label: string;
    if (s.kind === "imap") {
      const identity = account && account.imap
        ? `${friendlySourceLabel(s.label)} · ${account.imap.username} · ${account.imap.host}`
        : friendlySourceLabel(s.label);
      const n = (imapSeen.get(identity) ?? 0) + 1;
      imapSeen.set(identity, n);
      label = n === 1 ? identity : `${identity} · imap ${n}`;
    } else {
      const base = friendlySourceLabel(s.label);
      const n = (importSeen.get(base) ?? 0) + 1;
      importSeen.set(base, n);
      label = n === 1 ? `${base} · import` : `${base} · import ${n}`;
    }
    return { id: s.id, label, count: null, icon: iconOf(s.id), status: statusOf(s.id) };
  });
}

export interface RenderSourcesListOptions {
  onViewMessages: (account: AccountView) => void;
  onSyncNow: (account: AccountView) => void;
  onTogglePause: (account: AccountView) => void;
  onUpdateCredentials: (account: AccountView) => void;
  onForget: (account: AccountView) => void;
  onDisconnect: (account: AccountView) => void;
}

/** Render the Sources list (replaces children): one row per source —
 * title, kind, stats, sync state — with View messages (and Sync now for
 * email) as the primary actions, and pause/credentials/disconnect behind
 * a Details fold. Every control is a real focusable button with an
 * account-scoped accessible name. Disconnect is two-step: the first
 * click arms the confirmation, the second fires; a re-render disarms. */
export function renderSourcesList(
  el: HTMLElement,
  accounts: AccountView[],
  opts: RenderSourcesListOptions,
): void {
  if (accounts.length === 0) {
    const empty = document.createElement("li");
    empty.className = "account-row empty";
    const line = document.createElement("span");
    line.className = "meta";
    line.textContent = sourcesEmptyLine();
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
    main.append(iconEl(sourceIcon(account)));
    const title = document.createElement("span");
    title.className = "account-label";
    const displayName = accountTitle(account);
    title.textContent = displayName;
    const titleDetail = accountTitleDetail(account);
    if (titleDetail) title.title = titleDetail;
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

    const actions = document.createElement("div");
    actions.className = "account-actions";
    const view = document.createElement("button");
    view.type = "button";
    view.className = "source-view-btn";
    view.textContent = "View messages";
    view.title = `Show this source's messages in the archive (keeps your search text)`;
    view.setAttribute("aria-label", `View messages of source ${displayName}`);
    view.addEventListener("click", () => opts.onViewMessages(account));
    actions.append(view);

    if (account.kind === "imap") {
      const syncNow = document.createElement("button");
      syncNow.type = "button";
      syncNow.className = "account-sync-btn";
      syncNow.textContent = "Sync now";
      syncNow.title = `Sync ${displayName} now, reusing its stored credentials`;
      syncNow.setAttribute("aria-label", `Sync account ${displayName} now`);
      syncNow.addEventListener("click", () => opts.onSyncNow(account));
      actions.append(syncNow);
    }
    li.append(actions);

    if (account.kind === "imap") {
      const details = document.createElement("details");
      details.className = "source-details";
      const summary = document.createElement("summary");
      summary.textContent = "Details";
      details.append(summary);

      const pause = document.createElement("button");
      pause.type = "button";
      pause.className = "account-pause-btn";
      pause.textContent = account.excluded ? "Resume" : "Pause";
      pause.title = account.excluded
        ? `Include ${displayName} in auto-sync rounds again`
        : `Skip ${displayName} in auto-sync rounds (archive and cursors kept)`;
      pause.setAttribute(
        "aria-label",
        `${account.excluded ? "Resume" : "Pause"} account ${displayName}`,
      );
      pause.addEventListener("click", () => opts.onTogglePause(account));

      const update = document.createElement("button");
      update.type = "button";
      update.className = "source-update-credentials-btn";
      update.textContent = "Update credentials…";
      update.title = `Re-enter the password or Google sign-in for ${displayName}`;
      update.setAttribute("aria-label", `Update credentials of account ${displayName}`);
      update.addEventListener("click", () => opts.onUpdateCredentials(account));

      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "account-forget-btn";
      forget.textContent = "Forget credentials";
      forget.title = `Remove ${displayName}'s stored password and Google sign-in from the OS keyring (the archive stays)`;
      forget.setAttribute("aria-label", `Forget stored credentials of ${displayName}`);
      forget.addEventListener("click", () => opts.onForget(account));

      const disconnect = document.createElement("button");
      disconnect.type = "button";
      disconnect.className = "source-disconnect-btn";
      disconnect.textContent = "Disconnect…";
      disconnect.title = `Disconnect ${displayName}: stop syncing and remove its credentials — the archived messages stay`;
      disconnect.setAttribute(
        "aria-label",
        `Disconnect account ${displayName} (stops sync, removes credentials, keeps the archive)`,
      );
      let armed = false;
      disconnect.addEventListener("click", () => {
        if (!armed) {
          armed = true;
          disconnect.textContent = "Confirm disconnect";
          disconnect.title = `Really disconnect ${displayName}: stop syncing and remove its credentials — the archived messages stay`;
          return;
        }
        opts.onDisconnect(account);
      });

      details.append(pause, update, forget, disconnect);
      li.append(details);
    }

    return li;
  });
  el.replaceChildren(...rows);
}
