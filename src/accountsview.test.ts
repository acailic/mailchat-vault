// @vitest-environment happy-dom
// Unit tests for the sources view helpers (roadmap: the Sources list):
// the `sync_imap` request derivation (the acceptance-critical mapping
// from a stored account to the right sync parameters), the
// status/kind/stats wording, and the DOM builder (rows, primary actions,
// the Details fold, empty state) — all through textContent.
import { describe, expect, it, vi } from "vitest";
import type { AccountView, SourceCount, SourceInstance } from "./types";
import {
  accountKindLabel,
  accountStatsLine,
  accountTitle,
  accountTitleDetail,
  describeAccountSync,
  passwordStoreNote,
  renderSourcesList,
  sourceChipRows,
  sourcesEmptyLine,
  syncFailureText,
  syncNowRequest,
} from "./accountsview";

const imapAccount = (over: Partial<AccountView> = {}): AccountView => ({
  source_id: "inst-imap-1",
  kind: "imap",
  label: "imap:imap.gmail.com:993:ana@gmail.com",
  imap: {
    host: "imap.gmail.com",
    port: 993,
    username: "ana@gmail.com",
    folders: ["INBOX"],
    has_password: true,
    has_oauth: false,
  },
  message_count: 1234,
  failed_items: 0,
  state: "connected",
  excluded: false,
  last_attempt_at: "2026-01-02 10:00:00",
  last_success_at: "2026-01-02 10:00:05",
  last_error: null,
  last_error_at: null,
  ...over,
});

const importAccount = (over: Partial<AccountView> = {}): AccountView => ({
  source_id: "inst-import-1",
  kind: "import",
  label: "mbox:archive.mbox",
  imap: null,
  message_count: 7,
  failed_items: 0,
  state: "connected",
  excluded: false,
  last_attempt_at: null,
  last_success_at: null,
  last_error: null,
  last_error_at: null,
  ...over,
});

/** The row fixtures the DOM tests render: one email account, one import. */
const IMAP_ACCOUNT = imapAccount();
const IMPORT_ACCOUNT = importAccount();

describe("syncNowRequest", () => {
  it("derives the sync_imap request from the stored account", () => {
    const account = imapAccount();
    expect(syncNowRequest(account)).toEqual({
      host: "imap.gmail.com",
      port: 993,
      username: "ana@gmail.com",
      // Empty password: the backend resolves the stored keyring secret.
      password: "",
      folders: ["INBOX"],
      auth: "password",
    });
  });

  it("prefers the Google sign-in when no password is stored", () => {
    const account = imapAccount({
      imap: {
        host: "imap.gmail.com",
        port: 993,
        username: "ana@gmail.com",
        folders: [],
        has_password: false,
        has_oauth: true,
      },
    });
    const request = syncNowRequest(account)!;
    expect(request.auth).toBe("oauth");
    // Empty stored folders mean the connector's default set: pass null,
    // like the sync form does.
    expect(request.folders).toBeNull();
  });

  it("falls back to password mode for an account with no credentials", () => {
    const account = imapAccount({
      imap: {
        host: "imap.other.test",
        port: 993,
        username: "bob@other.test",
        folders: [],
        has_password: false,
        has_oauth: false,
      },
    });
    const request = syncNowRequest(account)!;
    expect(request.auth).toBe("password");
    expect(request.host).toBe("imap.other.test");
  });

  it("returns null for import accounts and imap rows without identity", () => {
    expect(syncNowRequest(importAccount())).toBeNull();
    expect(syncNowRequest(imapAccount({ imap: null }))).toBeNull();
  });
});

describe("labels and status lines", () => {
  it("labels the account kinds", () => {
    expect(accountKindLabel(imapAccount())).toBe("Email (IMAP)");
    expect(accountKindLabel(importAccount())).toBe("Import");
  });

  it("titles rows with the account label", () => {
    expect(accountTitle(importAccount())).toBe("mbox:archive.mbox");
  });

  it("titles an IMAP account by its address and server, never the raw connector label", () => {
    // §2.6: the stored `imap:host:port:user` label is an implementation
    // token; the row is titled by the identity the user knows.
    expect(accountTitle(imapAccount())).toBe("ana@gmail.com · imap.gmail.com");
    // The full stored label stays reachable as the title's tooltip for
    // support and bug reports — progressive disclosure, not the name.
    expect(accountTitleDetail(imapAccount())).toBe(
      "Stored source: imap:imap.gmail.com:993:ana@gmail.com",
    );
    // An IMAP-shaped row without a parsed identity falls back to the
    // label rather than crashing.
    expect(accountTitle(imapAccount({ imap: null }))).toBe(
      "imap:imap.gmail.com:993:ana@gmail.com",
    );
    // An import's label already is its user-facing name: no tooltip.
    expect(accountTitleDetail(importAccount())).toBe("");
  });

  it("describes a connected, synced email account with its credentials", () => {
    expect(describeAccountSync(imapAccount())).toBe(
      "connected · last synced 2026-01-02 10:00:05 · credentials: password",
    );
  });

  it("lists both credential kinds, pause, and never-synced states", () => {
    const oauthed = imapAccount({
      excluded: true,
      state: "disconnected",
      last_success_at: null,
      imap: {
        host: "imap.gmail.com",
        port: 993,
        username: "ana@gmail.com",
        folders: [],
        has_password: true,
        has_oauth: true,
      },
    });
    expect(describeAccountSync(oauthed)).toBe(
      "paused — auto-sync skips this account (archive kept) · never synced · credentials: password + Google sign-in",
    );
  });

  it("reports the failed-item backlog and import provenance", () => {
    expect(describeAccountSync(imapAccount({ failed_items: 2 }))).toContain(
      "2 failed items retrying",
    );
    expect(describeAccountSync(imapAccount({ failed_items: 1 }))).toContain(
      "1 failed item retrying",
    );
    expect(
      describeAccountSync(importAccount({ last_attempt_at: "2026-01-03 09:00:00" })),
    ).toBe("imported 2026-01-03 09:00:00");
    expect(describeAccountSync(importAccount())).toBe("imported");
  });

  it("leads with the recorded failure — what fell, when, and why (restart-durable state)", () => {
    const failed = imapAccount({
      state: "disconnected",
      last_success_at: null,
      last_error: "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)",
      last_error_at: "2026-11-09 23:00:00",
      imap: {
        host: "127.0.0.1",
        port: 59993,
        username: "first@h.invalid",
        folders: [],
        has_password: false,
        has_oauth: false,
      },
    });
    expect(describeAccountSync(failed)).toBe(
      "sync failed at 2026-11-09 23:00:00 — nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)" +
        " · never synced · no credentials stored",
    );
  });

  it("keeps pause ahead of a recorded failure", () => {
    const pausedAndFailed = imapAccount({
      excluded: true,
      last_error: "the server did not respond in time",
      last_error_at: "2026-11-09 23:00:00",
    });
    expect(describeAccountSync(pausedAndFailed)).toMatch(
      /^paused — auto-sync skips this account/,
    );
  });

  it("says which credentials are missing", () => {
    const bare = imapAccount({
      imap: {
        host: "imap.bare.test",
        port: 993,
        username: "c@bare.test",
        folders: [],
        has_password: false,
        has_oauth: false,
      },
    });
    expect(describeAccountSync(bare)).toContain("no credentials stored");
  });

  it("formats the stats line with the message count", () => {
    expect(accountStatsLine(imapAccount())).toBe("1,234 messages");
    expect(accountStatsLine(importAccount({ message_count: 1 }))).toBe("1 message");
    expect(accountStatsLine(imapAccount({ failed_items: 3 }))).toBe(
      "1,234 messages · 3 failing",
    );
  });

  it("words the empty state", () => {
    expect(sourcesEmptyLine()).toContain("No sources yet");
  });
});

describe("renderSourcesList", () => {
  function render(accounts: AccountView[]) {
    const el = document.createElement("ul");
    const opts = {
      onViewMessages: vi.fn(),
      onSyncNow: vi.fn(),
      onTogglePause: vi.fn(),
      onUpdateCredentials: vi.fn(),
      onForget: vi.fn(),
      onDisconnect: vi.fn(),
    };
    renderSourcesList(el, accounts, opts);
    return { el, opts };
  }

  it("shows View messages for every row and Sync now only for email", () => {
    const { el } = render([IMAP_ACCOUNT, IMPORT_ACCOUNT]);
    const rows = [...el.querySelectorAll(".account-row")];
    expect(rows.every((r) => r.querySelector(".source-view-btn"))).toBe(true);
    expect(rows[0].querySelector(".account-sync-btn")).toBeTruthy();
    expect(rows[1].querySelector(".account-sync-btn")).toBe(null);
  });

  it("management actions live inside the Details fold", () => {
    const { el, opts } = render([IMAP_ACCOUNT]);
    const details = el.querySelector("details.source-details")!;
    expect(details.querySelector(".account-pause-btn")).toBeTruthy();
    expect(details.querySelector(".source-update-credentials-btn")).toBeTruthy();
    expect(details.querySelector(".account-forget-btn")).toBeTruthy();
    (details.querySelector(".account-pause-btn") as HTMLElement).click();
    expect(opts.onTogglePause).toHaveBeenCalledWith(IMAP_ACCOUNT);
  });

  it("import rows have primary actions but no management fold", () => {
    const { el } = render([IMPORT_ACCOUNT]);
    expect(el.querySelector("details.source-details")).toBe(null);
    expect(el.querySelector(".source-view-btn")).toBeTruthy();
  });

  it("View messages reports its account", () => {
    const { el, opts } = render([IMPORT_ACCOUNT]);
    (el.querySelector(".source-view-btn") as HTMLElement).click();
    expect(opts.onViewMessages).toHaveBeenCalledWith(IMPORT_ACCOUNT);
  });

  it("Disconnect is two-step and resets on re-render", () => {
    const { el, opts } = render([IMAP_ACCOUNT]);
    const btn = el.querySelector(".source-disconnect-btn") as HTMLButtonElement;
    btn.click();
    expect(opts.onDisconnect).not.toHaveBeenCalled();
    expect(btn.textContent).toBe("Confirm disconnect");
    btn.click();
    expect(opts.onDisconnect).toHaveBeenCalledWith(IMAP_ACCOUNT);
    // Re-rendering the same list disarms the confirmation.
    renderSourcesList(el, [IMAP_ACCOUNT], opts);
    const again = el.querySelector(".source-disconnect-btn") as HTMLButtonElement;
    expect(again.textContent).toBe("Disconnect…");
  });

  it("renders the empty state as a single row", () => {
    const { el } = render([]);
    expect(el.children.length).toBe(1);
    expect(el.querySelector(".account-row.empty")!.textContent).toContain("No sources yet");
    expect(el.querySelectorAll("button").length).toBe(0);
  });

  it("escapes nothing by construction: labels land via textContent", () => {
    const evil = imapAccount({ label: '<script>alert("x")</script>' });
    const { el } = render([evil]);
    // The stored label is disclosed only as the title's tooltip, and
    // still lands as an attribute value — never markup.
    expect((el.querySelector(".account-label") as HTMLElement).title).toBe(
      `Stored source: ${evil.label}`,
    );
    expect(el.querySelector("script")).toBeNull();
  });
});

describe("syncFailureText / passwordStoreNote (the honest failure surface)", () => {
  it("uses the structured failure's classified message", () => {
    expect(
      syncFailureText({
        class: "connection_refused",
        message:
          "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)",
        saved_to: null,
      }),
    ).toBe(
      "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)",
    );
  });

  it("says where the entered password already lives on a failed sync (memory → restart clears it)", () => {
    // The password is saved BEFORE the connection is attempted, so a
    // failed sync can still have stored one — the user must be told.
    expect(
      syncFailureText({
        class: "connection_refused",
        message: "nothing answered at that host and port",
        saved_to: "memory",
      }),
    ).toBe(
      "nothing answered at that host and port Password kept in memory only for this session (no OS keyring available) — restarting the app clears it.",
    );
    expect(
      syncFailureText({ class: "timeout", message: "the server did not respond", saved_to: "os_keyring" }),
    ).toBe("the server did not respond Password stored in the OS keyring.");
  });

  it("falls back to the string itself for plain-string rejections", () => {
    expect(syncFailureText("no password stored for this account — enter one")).toBe(
      "no password stored for this account — enter one",
    );
  });

  it("extracts the message of a plain Error rejection", () => {
    // A thrown Error (not the structured shape) still renders its
    // message, not "Error: boom".
    expect(syncFailureText(new Error("boom"))).toBe("boom");
  });

  it("passwordStoreNote states the restart rule for each store", () => {
    expect(passwordStoreNote("memory")).toContain("restarting the app clears it");
    // "os_keyring" quoted directly as the argument of a password-prefixed
    // call reads as a credential literal to secret scanners (GitGuardian
    // false positive, PR 71) — bind the store name first; the assertion
    // sentence itself scans clean.
    const osKeyringStore = "os_keyring";
    const keyringNote = passwordStoreNote(osKeyringStore);
    expect(keyringNote).toBe("Password stored in the OS keyring.");
    expect(passwordStoreNote(null)).toBe("");
    expect(passwordStoreNote(undefined)).toBe("");
  });
});

describe("sourceChipRows", () => {
  const inst = (id: string, label: string): SourceInstance => ({
    id, kind: "legacy", label, connector_key: "mbox", config_json: "{}",
    state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, created_at: "2026-01-01 00:00:00",
  });
  const imap = (id: string, label: string, user: string, host: string): AccountView => ({
    source_id: id, kind: "imap", label, imap: {
      host, port: 993, username: user, folders: [], has_password: true, has_oauth: false,
    }, message_count: 0, failed_items: 0, state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, last_error: null, last_error_at: null,
  });
  const imp = (id: string, label: string): AccountView => ({
    source_id: id, kind: "import", label, imap: null,
    message_count: 0, failed_items: 0, state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, last_error: null, last_error_at: null,
  });

  it("one row per instance, keyed by id, with the label's count when unique", () => {
    const counts: SourceCount[] = [
      { source: "mbox", count: 3 },
      { source: "demo", count: 5 },
    ];
    const rows = sourceChipRows(
      [inst("a", "mbox"), inst("b", "demo")],
      [imp("a", "mbox"), imp("b", "demo")],
      counts,
    );
    expect(rows).toEqual([
      { id: "a", label: "mbox", count: 3 },
      { id: "b", label: "demo", count: 5 },
    ]);
  });

  it("duplicate labels are disambiguated and lose the shared count", () => {
    const rows = sourceChipRows(
      [inst("a", "chat"), inst("b", "chat"), inst("c", "chat")],
      [imp("a", "chat"), imap("b", "chat", "ana@gmail.com", "imap.gmail.com"), imp("c", "chat")],
      [{ source: "chat", count: 9 }],
    );
    expect(rows.map((r) => r.id)).toEqual(["a", "b", "c"]);
    expect(rows.every((r) => r.count === null)).toBe(true);
    expect(rows.map((r) => r.label)).toEqual([
      "chat · import",
      "chat · ana@gmail.com · imap.gmail.com",
      "chat · import 2",
    ]);
  });

  it("instances without an account row still appear (id, label, label count)", () => {
    const rows = sourceChipRows([inst("x", "solo")], [], [{ source: "solo", count: 1 }]);
    expect(rows).toEqual([{ id: "x", label: "solo", count: 1 }]);
  });
});
