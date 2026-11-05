// Unit tests for the desktop-notification layer (src/notify.ts): digest
// extraction from sync/watch results, notification text, and the
// notifier's gates — enabled, focused, permission, and the never-throw
// contract. Pure logic; the plugin itself is never touched (the channel
// is always a fake).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createNewMailNotifier,
  digestFromJobResult,
  digestFromSyncFolders,
  digestFromWatchEvent,
  notificationBody,
  notificationTitle,
  type NewMailNotifierDeps,
  type NotifyChannel,
} from "./notify";
import type { JobEvent, WatchEvent } from "./types";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function syncFolders(inserted: number, folder = "INBOX") {
  return {
    folder,
    uidvalidity: 1,
    reset: false,
    seen: inserted + 2,
    inserted,
    skipped: 2,
    failed: 0,
    resolved_failures: 0,
    conflicting_revisions: 0,
    error: null,
  };
}

function jobEvent(seq: number, result: unknown, type = "succeeded"): JobEvent {
  return {
    seq,
    at: "2026-11-05T10:00:00Z",
    job: 7,
    type,
    result,
  } as JobEvent;
}

function watchImported(inserted: number, path = "/tmp/exports/chat.txt"): WatchEvent {
  return {
    kind: "imported",
    path,
    summary: { source: "watch", seen: inserted, inserted, skipped: 0, failed: 0 },
  };
}

function roundResult(...sources: Array<Array<ReturnType<typeof syncFolders>>>) {
  return {
    outcomes: sources.map((folders, i) => ({
      instance_id: `inst-${i}`,
      label: `imap:mail.example.com:993:user${i}@example.com`,
      status: { status: "synced", folders },
    })),
    excluded: [],
  };
}

// ---------------------------------------------------------------------------
// The fake channel
// ---------------------------------------------------------------------------

interface ChannelScript {
  granted?: boolean;
  requestAnswer?: boolean | Error;
  sends: Array<{ title: string; body: string }>;
}

function fakeChannel(script: ChannelScript): NotifyChannel & { script: ChannelScript } {
  return {
    script,
    isPermissionGranted: () => script.granted ?? true,
    requestPermission: () => {
      if (script.requestAnswer instanceof Error) throw script.requestAnswer;
      return script.requestAnswer ?? true;
    },
    send: (title, body) => script.sends.push({ title, body }),
  };
}

function notifierWith(
  script: ChannelScript,
  opts: { enabled?: boolean; focused?: boolean } = {},
) {
  const deps: NewMailNotifierDeps = {
    channel: fakeChannel(script),
    isFocused: () => opts.focused ?? false,
    isEnabled: () => opts.enabled ?? true,
  };
  return createNewMailNotifier(deps);
}

// The notifier's deliver() is fire-and-forget async; flush the microtask
// queue before asserting on sends.
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

let errorSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  errorSpy.mockRestore();
});

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

describe("digestFromSyncFolders", () => {
  it("sums inserts across folders and keeps only folders with new mail", () => {
    const digest = digestFromSyncFolders([
      syncFolders(2, "INBOX"),
      syncFolders(0, "[Gmail]/Sent Mail"),
      syncFolders(3, "Archive"),
    ]);
    expect(digest).toEqual({
      count: 5,
      folders: ["INBOX", "Archive"],
      file: null,
    });
  });

  it("returns null when a cycle stored nothing new", () => {
    expect(digestFromSyncFolders([syncFolders(0, "INBOX"), syncFolders(0)])).toBeNull();
    expect(digestFromSyncFolders([])).toBeNull();
  });
});

describe("digestFromJobResult", () => {
  it("reads an imap_sync result (folder summary array)", () => {
    const digest = digestFromJobResult("imap_sync", [syncFolders(4, "INBOX")]);
    expect(digest?.count).toBe(4);
    expect(digest?.folders).toEqual(["INBOX"]);
  });

  it("merges every synced source of an imap_sync_all round", () => {
    const digest = digestFromJobResult(
      "imap_sync_all",
      roundResult(
        [syncFolders(1, "INBOX")],
        [syncFolders(2, "INBOX"), syncFolders(0, "Sent")],
        [], // a source with no new mail contributes nothing
      ),
    );
    expect(digest).toEqual({ count: 3, folders: ["INBOX", "INBOX"], file: null });
  });

  it("ignores failed and credential-less sources", () => {
    const result = {
      outcomes: [
        { instance_id: "a", label: "a", status: { status: "synced", folders: [syncFolders(1)] } },
        { instance_id: "b", label: "b", status: { status: "failed", error: "boom" } },
        { instance_id: "c", label: "c", status: { status: "no_credentials" } },
      ],
      excluded: [],
    };
    expect(digestFromJobResult("imap_sync_all", result)?.count).toBe(1);
  });

  it("returns null for other job kinds and unexpected shapes", () => {
    expect(digestFromJobResult("embed_messages", {})).toBeNull();
    expect(digestFromJobResult("import_mbox", { weird: true })).toBeNull();
    expect(digestFromJobResult("imap_sync", { not: "an array" })).toBeNull();
    expect(digestFromJobResult("imap_sync_all", { outcomes: "nope" })).toBeNull();
    expect(digestFromJobResult(undefined, null)).toBeNull();
  });
});

describe("digestFromWatchEvent", () => {
  it("carries the inserted count and the file name only", () => {
    const digest = digestFromWatchEvent(watchImported(6, "/home/u/exports/telegram/result.json"));
    expect(digest).toEqual({ count: 6, folders: [], file: "result.json" });
  });

  it("ignores non-import events and imports that stored nothing new", () => {
    expect(digestFromWatchEvent({ kind: "folder_added", path: "/tmp" })).toBeNull();
    expect(digestFromWatchEvent(watchImported(0))).toBeNull();
    expect(digestFromWatchEvent(undefined)).toBeNull();
  });
});

describe("notification text", () => {
  it("titles with the count, singular and plural", () => {
    expect(notificationTitle({ count: 1, folders: ["INBOX"], file: null })).toBe("1 new message");
    expect(notificationTitle({ count: 5, folders: ["INBOX"], file: null })).toBe(
      "5 new messages",
    );
  });

  it("lists the folders that received mail, folding long lists", () => {
    expect(notificationBody({ count: 3, folders: ["INBOX", "Archive"], file: null })).toBe(
      "INBOX, Archive",
    );
    expect(
      notificationBody({ count: 9, folders: ["INBOX", "A", "B", "C", "D"], file: null }),
    ).toBe("INBOX, A, B +2 more");
  });

  it("names the imported file for watched imports", () => {
    expect(notificationBody({ count: 2, folders: [], file: "chat.txt" })).toBe(
      "imported from chat.txt",
    );
  });
});

// ---------------------------------------------------------------------------
// The notifier gates
// ---------------------------------------------------------------------------

describe("createNewMailNotifier", () => {
  it("sends one native notification for a new-mail sync round", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script);
    notifier.onJobEvent(jobEvent(3, [syncFolders(2, "INBOX")]), "imap_sync");
    await flush();
    expect(script.sends).toEqual([{ title: "2 new messages", body: "INBOX" }]);
  });

  it("stays silent while the app window is focused (the user is reading)", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script, { focused: true });
    notifier.onJobEvent(jobEvent(3, [syncFolders(2, "INBOX")]), "imap_sync");
    notifier.onSyncOutcome([syncFolders(5)]);
    notifier.onWatchSnapshot([watchImported(4)]);
    await flush();
    expect(script.sends).toEqual([]);
  });

  it("stays silent when the Settings option is off", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script, { enabled: false });
    notifier.onJobEvent(jobEvent(1, roundResult([syncFolders(1)])), "imap_sync_all");
    await flush();
    expect(script.sends).toEqual([]);
  });

  it("stays silent for cycles that stored nothing new", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script);
    notifier.onJobEvent(jobEvent(1, [syncFolders(0)]), "imap_sync");
    notifier.onJobEvent(jobEvent(2, roundResult([])), "imap_sync_all");
    notifier.onWatchSnapshot([watchImported(0)]);
    notifier.onWatchSnapshot([{ kind: "skipped", path: "/tmp/x.txt", reason: "unknown format" }]);
    await flush();
    expect(script.sends).toEqual([]);
  });

  it("requests permission once when not yet granted, then sends", async () => {
    const script: ChannelScript = { granted: false, requestAnswer: true, sends: [] };
    const notifier = notifierWith(script);
    notifier.onSyncOutcome([syncFolders(1, "INBOX")]);
    await flush();
    expect(script.sends).toEqual([{ title: "1 new message", body: "INBOX" }]);
  });

  it("never sends when permission is denied", async () => {
    const script: ChannelScript = { granted: false, requestAnswer: false, sends: [] };
    const notifier = notifierWith(script);
    notifier.onSyncOutcome([syncFolders(1)]);
    await flush();
    expect(script.sends).toEqual([]);
  });

  it("swallows channel failures without throwing into the UI flow", async () => {
    const script: ChannelScript = { requestAnswer: new Error("no dbus"), sends: [] };
    script.granted = false;
    const notifier = notifierWith(script);
    expect(() => notifier.onSyncOutcome([syncFolders(1)])).not.toThrow();
    await flush();
    expect(script.sends).toEqual([]);
    expect(errorSpy).not.toHaveBeenCalled();
  });

  it("ignores stale/replayed job events (seq guard)", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script);
    notifier.onJobEvent(jobEvent(4, [syncFolders(1)]), "imap_sync");
    await flush();
    expect(script.sends.length).toBe(1);
    // The same success replayed (older or equal seq) must not re-notify.
    notifier.onJobEvent(jobEvent(4, [syncFolders(1)]), "imap_sync");
    notifier.onJobEvent(jobEvent(3, [syncFolders(1)]), "imap_sync");
    await flush();
    expect(script.sends.length).toBe(1);
  });

  it("only notifies succeeded jobs; failures are not notifications", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script);
    const started = { seq: 1, at: "t", job: 7, type: "started" } as JobEvent;
    const failed = {
      seq: 2,
      at: "t",
      job: 7,
      type: "failed",
      error: { kind: "io", message: "boom" },
    } as unknown as JobEvent;
    notifier.onJobEvent(started, "imap_sync");
    notifier.onJobEvent(failed, "imap_sync");
    await flush();
    expect(script.sends).toEqual([]);
  });

  it("notifies a watched import once even as its event stays newest", async () => {
    const script: ChannelScript = { sends: [] };
    const notifier = notifierWith(script);
    const event = watchImported(3);
    notifier.onWatchSnapshot([event]);
    // Later snapshots still list it as the newest event until something
    // newer happens — no duplicate notifications.
    notifier.onWatchSnapshot([event]);
    notifier.onWatchSnapshot([event]);
    await flush();
    expect(script.sends).toEqual([{ title: "3 new messages", body: "imported from chat.txt" }]);
    // A genuinely new import notifies again.
    notifier.onWatchSnapshot([watchImported(5), event]);
    await flush();
    expect(script.sends.length).toBe(2);
    expect(script.sends[1]).toEqual({ title: "5 new messages", body: "imported from chat.txt" });
  });

  it("requests permission when the setting is toggled on (a user gesture)", async () => {
    const script: ChannelScript = { granted: false, requestAnswer: true, sends: [] };
    const notifier = notifierWith(script);
    notifier.onSettingToggled(true);
    await flush();
    expect(script.sends).toEqual([]); // the toggle only asks, never sends
    // Permission sticks: the next new-mail round sends without asking again.
    notifier.onSyncOutcome([syncFolders(2, "INBOX")]);
    await flush();
    expect(script.sends).toEqual([{ title: "2 new messages", body: "INBOX" }]);
  });

  it("does not ask for permission when the setting is toggled off", async () => {
    const script: ChannelScript = { granted: false, sends: [] };
    let asked = 0;
    const channel = fakeChannel(script);
    const base = channel.requestPermission;
    channel.requestPermission = () => {
      asked += 1;
      return base();
    };
    const notifier = createNewMailNotifier({
      channel,
      isFocused: () => false,
      isEnabled: () => true,
    });
    notifier.onSettingToggled(false);
    await flush();
    expect(asked).toBe(0);
  });
});
