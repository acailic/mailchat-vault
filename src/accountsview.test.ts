// @vitest-environment happy-dom
// Unit tests for the multi-account view helpers (roadmap: the accounts
// panel): the `sync_imap` request derivation (the acceptance-critical
// mapping from a stored account to the right sync parameters), the
// status/kind/stats wording, and the DOM builder (rows, focusable action
// buttons, empty state) — all through textContent.
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountView } from "./types";
import {
  accountKindLabel,
  accountStatsLine,
  accountsEmptyLine,
  accountTitle,
  describeAccountSync,
  renderAccounts,
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
  ...over,
});

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
    expect(accountsEmptyLine()).toContain("No accounts yet");
  });
});

describe("renderAccounts", () => {
  let el: HTMLElement;
  beforeEach(() => {
    el = document.createElement("ul");
  });

  it("renders the empty state as a single row", () => {
    renderAccounts(el, [], {
      onSyncNow: () => {},
      onTogglePause: () => {},
      onForget: () => {},
    });
    expect(el.children.length).toBe(1);
    expect(el.querySelector(".account-row.empty")!.textContent).toContain("No accounts yet");
    expect(el.querySelectorAll("button").length).toBe(0);
  });

  it("renders one row per account with kind, stats, and sync state", () => {
    renderAccounts(el, [imapAccount(), importAccount()], {
      onSyncNow: () => {},
      onTogglePause: () => {},
      onForget: () => {},
    });
    const rows = [...el.querySelectorAll(".account-row")];
    expect(rows.length).toBe(2);
    expect((rows[0] as HTMLElement).dataset.sourceId).toBe("inst-imap-1");
    expect(rows[0].querySelector(".account-label")!.textContent).toBe(
      "imap:imap.gmail.com:993:ana@gmail.com",
    );
    expect(rows[0].querySelector(".account-kind")!.textContent).toBe("Email (IMAP)");
    expect(rows[0].querySelector(".account-stats")!.textContent).toBe("1,234 messages");
    expect(rows[0].querySelector(".account-sync")!.textContent).toContain(
      "last synced 2026-01-02 10:00:05",
    );
    expect((rows[1] as HTMLElement).dataset.sourceId).toBe("inst-import-1");
    expect(rows[1].querySelector(".account-kind")!.textContent).toBe("Import");
  });

  it("gives email accounts the three management actions, imports none", () => {
    renderAccounts(el, [imapAccount(), importAccount()], {
      onSyncNow: () => {},
      onTogglePause: () => {},
      onForget: () => {},
    });
    const imapRow = el.querySelector('.account-row[data-source-id="inst-imap-1"]')!;
    const buttons = [...imapRow.querySelectorAll("button")];
    expect(buttons.map((b) => b.textContent)).toEqual([
      "Sync now",
      "Pause",
      "Forget credentials",
    ]);
    // Every action is a focusable button with an account-scoped label.
    for (const button of buttons) {
      expect(button.getAttribute("aria-label")).toContain("ana@gmail.com");
    }
    const importRow = el.querySelector('.account-row[data-source-id="inst-import-1"]')!;
    expect(importRow.querySelectorAll("button").length).toBe(0);
  });

  it("flips the pause button for a paused account", () => {
    renderAccounts(el, [imapAccount({ excluded: true })], {
      onSyncNow: () => {},
      onTogglePause: () => {},
      onForget: () => {},
    });
    const pause = el.querySelector<HTMLButtonElement>(".account-pause-btn")!;
    expect(pause.textContent).toBe("Resume");
  });

  it("wires every action to its callback with the account", () => {
    const onSyncNow = vi.fn();
    const onTogglePause = vi.fn();
    const onForget = vi.fn();
    renderAccounts(el, [imapAccount()], { onSyncNow, onTogglePause, onForget });
    (el.querySelector(".account-sync-btn") as HTMLButtonElement).click();
    (el.querySelector(".account-pause-btn") as HTMLButtonElement).click();
    (el.querySelector(".account-forget-btn") as HTMLButtonElement).click();
    expect(onSyncNow).toHaveBeenCalledWith(imapAccount());
    expect(onTogglePause).toHaveBeenCalledWith(imapAccount());
    expect(onForget).toHaveBeenCalledWith(imapAccount());
  });

  it("escapes nothing by construction: labels land via textContent", () => {
    const evil = imapAccount({ label: '<script>alert("x")</script>' });
    renderAccounts(el, [evil], {
      onSyncNow: () => {},
      onTogglePause: () => {},
      onForget: () => {},
    });
    expect(el.querySelector(".account-label")!.textContent).toBe(evil.label);
    expect(el.querySelector("script")).toBeNull();
  });
});
