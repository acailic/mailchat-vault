// Unit tests for the pure backup/restore view helpers (Settings →
// Storage & backup, M4.4). Mirrors the vitest style of the other view
// modules.

import { describe, expect, it } from "vitest";

import {
  backupDoneLine,
  canCommit,
  countsLine,
  gateBusy,
  GATE_BUSY_MESSAGE,
  humanBytes,
  lastBackupLine,
  manifestLine,
  noBackupLine,
  previewLine,
  recoveryLine,
  restoreDoneLine,
  switchStopped,
  SWITCH_STOPPED_MARKER,
  slotLine,
  vaultOriginLine,
  type BackupManifest,
  type LastBackupView,
  type LastSwitchView,
  type RecoveryStatusView,
  type RestoreOutcomeView,
  type RestorePreviewView,
} from "./backupview";

const manifest: BackupManifest = {
  format_version: 1,
  schema_version: 12,
  created_at: "2026-11-27T15:30:45Z",
  counts: { messages: 1234, sources: 3, media_objects: 57 },
  db: { path: "vault.db", size: 999, sha256: "a".repeat(64) },
  settings: { path: "settings.json", size: 321, sha256: "b".repeat(64) },
  objects: [
    { path: "media/".padEnd(71, "c") + ".jpg", size: 10, sha256: "c".repeat(64) },
    { path: "media/".padEnd(71, "d") + ".bin", size: 20, sha256: "d".repeat(64) },
  ],
};

describe("humanBytes", () => {
  it("formats binary units with one decimal below 10", () => {
    expect(humanBytes(0)).toBe("0 B");
    expect(humanBytes(512)).toBe("512 B");
    expect(humanBytes(2048)).toBe("2 KiB");
    expect(humanBytes(5 * 1024 * 1024)).toBe("5 MiB");
    expect(humanBytes(1.5 * 1024 * 1024 * 1024)).toBe("1.5 GiB");
    expect(humanBytes(16 * 1024 ** 3)).toBe("16 GiB");
  });
});

describe("countsLine / manifestLine", () => {
  it("pluralizes honestly", () => {
    expect(countsLine({ messages: 1, sources: 1, media_objects: 1 })).toBe(
      "1 message · 1 source · 1 media object",
    );
    expect(countsLine({ messages: 2, sources: 3, media_objects: 4 })).toBe(
      "2 messages · 3 sources · 4 media objects",
    );
  });

  it("summarizes a container", () => {
    expect(manifestLine(manifest)).toBe(
      "Backup from 2026-11-27T15:30:45Z: 1234 messages · 3 sources · 57 media objects · 2 objects carried · settings included",
    );
    const bare = { ...manifest, settings: null, objects: [] };
    expect(manifestLine(bare)).toContain("no settings file");
    expect(manifestLine(bare)).toContain("0 objects carried");
  });
});

describe("job and preview lines", () => {
  it("backupDoneLine names the destination and size", () => {
    expect(backupDoneLine({ path: "/x/vault.mcbackup", size: 4096, manifest })).toBe(
      "Backup written to /x/vault.mcbackup (4 KiB).",
    );
  });

  const preview: RestorePreviewView = {
    preview_id: "abcd",
    archive: "/x/vault.mcbackup",
    manifest,
    restored_settings: true,
    objects: 2,
  };

  it("previewLine states what was validated", () => {
    expect(previewLine(preview)).toContain("validated");
    expect(previewLine(preview)).toContain("settings will be restored");
    expect(previewLine({ ...preview, restored_settings: false })).toContain(
      "no settings in this backup",
    );
  });

  const outcome: RestoreOutcomeView = {
    manifest,
    previous_slot: "previous-20261127153045-abc123",
    restored_settings: true,
    sources_needing_reauth: 2,
  };

  it("switchStopped matches the exact backend marker, never prose", () => {
    // The stable Rust↔frontend contract: only the exact leading token
    // counts — a wording change on either side cannot silently flip a
    // real mid-switch failure into the usable-app refusal branch.
    expect(switchStopped(`${SWITCH_STOPPED_MARKER} restart required — details`)).toBe(true);
    expect(switchStopped(new Error(`${SWITCH_STOPPED_MARKER} restart required`))).toBe(true);
    // Not at the start, or merely mentioning the words: refused.
    expect(switchStopped("restore refused: some switch_stopped: mid-token")).toBe(false);
    expect(switchStopped("restart required — but no marker")).toBe(false);
    expect(switchStopped("a vault switch is already in progress")).toBe(false);
    expect(switchStopped(null)).toBe(false);
  });

  it("gateBusy matches the backend gate refusal word-for-word", () => {
    // The shared contract with the switch gate's refusal (pinned on the
    // Rust side): only the exact message counts — a gate refusal is
    // transient, so the commit flow distinguishes it from final refusals.
    expect(gateBusy(GATE_BUSY_MESSAGE)).toBe(true);
    expect(gateBusy(new Error(GATE_BUSY_MESSAGE))).toBe(true);
    expect(gateBusy(`restore refused: ${GATE_BUSY_MESSAGE}`)).toBe(false);
    expect(gateBusy("a vault switch or backup is already in progress")).toBe(false);
    expect(gateBusy(null)).toBe(false);
  });

  it("restoreDoneLine asks to reconnect only what the keyring lacks", () => {
    const line = restoreDoneLine(outcome);
    expect(line).toContain("Restart the app");
    expect(line).toContain("reconnect 2 of 3 restored accounts");
    expect(line).toContain("passwords are never restored");
    expect(line).toContain("previous-20261127153045-abc123");
    // Same-machine restore with live keyring entries (0): restart only —
    // no reconnection clause and no "passwords are never restored" claim.
    const sameMachine = restoreDoneLine({ ...outcome, sources_needing_reauth: 0 });
    expect(sameMachine).not.toContain("reconnect");
    expect(sameMachine).not.toContain("passwords are never restored");
    expect(sameMachine).toContain("Restart the app");
    // One of three accounts missing: singular, qualified by the total.
    expect(restoreDoneLine({ ...outcome, sources_needing_reauth: 1 })).toContain(
      "reconnect 1 of 3 restored accounts",
    );
    // Cross-machine restore (every source): the plain count, no "of".
    const crossMachine = restoreDoneLine({ ...outcome, sources_needing_reauth: 3 });
    expect(crossMachine).toContain("reconnect 3 restored accounts");
    expect(crossMachine).not.toContain("of 3 restored");
  });
});

describe("recovery lines", () => {
  it("describes an empty recovery state", () => {
    const empty: RecoveryStatusView = {
      journal_state: null,
      journal_created_at: null,
      staged_preview: false,
      previous: [],
      last_switch: null,
      data_dir: "/home/me/.local/share/mailchat-vault",
    };
    expect(recoveryLine(empty)).toContain("No previous-vault snapshots");
  });

  it("counts slots and flags unresolved journals and staged previews", () => {
    const status: RecoveryStatusView = {
      journal_state: "completed",
      journal_created_at: "2026-11-27T15:30:45Z",
      staged_preview: true,
      previous: [
        { id: "previous-20261127153045-abc123", created_at: "2026-11-27T15:30:45Z", total_bytes: 1024, files: 3 },
      ],
      last_switch: null,
      data_dir: "/home/me/.local/share/mailchat-vault",
    };
    const line = recoveryLine(status);
    expect(line).toContain("1 previous-vault snapshot kept:");
    // No session preview behind it: the staged bytes are from an earlier
    // session — the headline directs to re-validation, never claims a
    // committable restore the UI cannot commit.
    expect(line).toContain("previews are session-bound");
    expect(line).not.toContain("waiting to be committed");
    expect(line).not.toContain("unresolved");
    // With this session's validated preview behind it, the commit claim
    // is true.
    expect(recoveryLine(status, true)).toContain("validated restore is waiting");
    const unresolved = recoveryLine({
      ...status,
      journal_state: "staged",
      staged_preview: false,
      previous: [
        status.previous[0],
        { id: "previous-20261127160000-def012", created_at: "2026-11-27T16:00:00Z", total_bytes: 2048, files: 4 },
      ],
    });
    expect(unresolved).toContain("2 previous-vault snapshots kept:");
    expect(unresolved).toContain("unresolved");
  });

  it("slotLine shows date, size, and file count", () => {
    expect(
      slotLine({
        id: "previous-20261127153045-abc123",
        created_at: "2026-11-27T15:30:45Z",
        total_bytes: 2 * 1024 * 1024,
        files: 12,
      }),
    ).toBe("2026-11-27T15:30:45Z · 2 MiB · 12 files");
  });
});

describe("lastBackupLine (U05: persisted last verified backup)", () => {
  const record: LastBackupView = {
    created_at: "2026-11-27T15:30:45Z",
    path: "/media/vault-2026-11-27.mcbackup",
    size: 2048,
    counts: { messages: 1234, sources: 3, media_objects: 57 },
    still_present: true,
  };

  it("names when, what, where, and how big — the panel's boot line", () => {
    const line = lastBackupLine(record);
    expect(line).toContain("Last verified backup: 2026-11-27T15:30:45Z");
    expect(line).toContain("1234 messages · 3 sources · 57 media objects");
    expect(line).toContain("/media/vault-2026-11-27.mcbackup");
    expect(line).toContain("2 KiB");
    expect(line).not.toContain("no longer");
  });

  it("honestly flags a container the user moved or deleted", () => {
    expect(lastBackupLine({ ...record, still_present: false })).toContain(
      "the file is no longer at that path",
    );
  });

  it("the empty state explains what the line means", () => {
    expect(noBackupLine()).toContain("No verified backup yet");
    expect(noBackupLine()).toContain("survives restarts");
  });
});

describe("vaultOriginLine (U05: restart after restore/return)", () => {
  const restored: LastSwitchView = {
    kind: "restored",
    archive: "/media/vault-2026-11-27.mcbackup",
    since: "2026-11-27T15:30:45Z",
    recovery: "none",
  };

  it("says what is active, since when, from where", () => {
    expect(vaultOriginLine(restored)).toBe(
      "Active vault: switched in from the backup /media/vault-2026-11-27.mcbackup at 2026-11-27T15:30:45Z.",
    );
    // A return has no archive — the origin is a previous-vault snapshot.
    expect(vaultOriginLine({ ...restored, kind: "returned", archive: null })).toBe(
      "Active vault: switched in from a previous-vault snapshot at 2026-11-27T15:30:45Z.",
    );
  });

  it("a rollback resolved at startup says which vault is active", () => {
    const line = vaultOriginLine({ ...restored, recovery: "rolled_back" });
    expect(line).toContain("interrupted and rolled back at startup");
    expect(line).toContain("the vault from before it is active");
  });

  it("a roll-forward resolved at startup names the switched-in vault", () => {
    const line = vaultOriginLine({ ...restored, recovery: "rolled_forward" });
    expect(line).toContain("interrupted and completed at startup");
    expect(line).toContain("the switched-in vault is active");
  });
});

describe("canCommit", () => {
  it("allows committing only from the validated state", () => {
    expect(canCommit({ step: "idle" })).toBe(false);
    expect(canCommit({ step: "inspected", archive: "x", manifest })).toBe(false);
    expect(canCommit({ step: "validating", archive: "x" })).toBe(false);
    expect(
      canCommit({
        step: "validated",
        preview: { preview_id: "p", archive: "x", manifest, restored_settings: false, objects: 0 },
      }),
    ).toBe(true);
    expect(
      canCommit({
        step: "committing",
        preview: { preview_id: "p", archive: "x", manifest, restored_settings: false, objects: 0 },
      }),
    ).toBe(false);
    expect(canCommit({ step: "done", outcome: null as never })).toBe(false);
  });
});
