// Pure backup/restore helpers and types (unit tested, no DOM wiring):
// mirrors `vault_core::backup` for the Settings "Storage & backup" UI.

/** Mirrors `vault_core::backup::BackupCounts` (serde snake_case). */
export interface BackupCounts {
  messages: number;
  sources: number;
  media_objects: number;
}

/** Mirrors `vault_core::backup::BackupManifest` (the inspect/preview view). */
export interface BackupManifest {
  format_version: number;
  schema_version: number;
  created_at: string;
  counts: BackupCounts;
  db: { path: string; size: number; sha256: string };
  settings: { path: string; size: number; sha256: string } | null;
  objects: Array<{ path: string; size: number; sha256: string }>;
}

/** The `backup_create` job's result (mirrors `vault_core::backup::BackupSummary`). */
export interface BackupSummaryView {
  path: string;
  size: number;
  manifest: BackupManifest;
}

/** The `restore_preview` job's result (mirrors `vault_core::backup::RestorePreview`). */
export interface RestorePreviewView {
  preview_id: string;
  archive: string;
  manifest: BackupManifest;
  restored_settings: boolean;
  objects: number;
}

/** The `restore_commit` / `return_to_previous_vault` result
 *  (mirrors `vault_core::backup::RestoreOutcome`). */
export interface RestoreOutcomeView {
  manifest: BackupManifest;
  previous_slot: string;
  restored_settings: boolean;
  sources_needing_reauth: number;
}

/** Mirrors `vault_core::backup::SwitchState` (serde snake_case). */
export type SwitchState = "switching" | "staged" | "completed";

/** Mirrors `vault_core::backup::PreviousVaultSlot`. */
export interface PreviousVaultSlot {
  id: string;
  created_at: string;
  total_bytes: number;
  files: number;
}

/** Mirrors `vault_core::backup::RecoveryStatus`. */
export interface RecoveryStatusView {
  journal_state: SwitchState | null;
  journal_created_at: string | null;
  staged_preview: boolean;
  previous: PreviousVaultSlot[];
}

/** The restore flow's UI state machine (pure transitions; the wiring in
 *  main.ts drives it). */
export type RestoreFlow =
  | { step: "idle" }
  | { step: "inspected"; archive: string; manifest: BackupManifest }
  | { step: "validating"; archive: string }
  | { step: "validated"; preview: RestorePreviewView }
  | { step: "committing"; preview: RestorePreviewView }
  | { step: "done"; outcome: RestoreOutcomeView }
  | { step: "error"; message: string };

/** Human byte sizes (binary units, one decimal below 10). */
export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = n;
  let unit = -1;
  do {
    value /= 1024;
    unit += 1;
  } while (value >= 1024 && unit < units.length - 1);
  const text = value >= 10 || Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1);
  return `${text} ${units[unit]}`;
}

/** Pluralized "N messages · N sources · N media objects" summary. */
export function countsLine(counts: BackupCounts): string {
  const messages = counts.messages === 1 ? "1 message" : `${counts.messages} messages`;
  const sources = counts.sources === 1 ? "1 source" : `${counts.sources} sources`;
  const objects =
    counts.media_objects === 1 ? "1 media object" : `${counts.media_objects} media objects`;
  return `${messages} · ${sources} · ${objects}`;
}

/** One-line summary of an inspected/validated container. */
export function manifestLine(manifest: BackupManifest): string {
  const settings = manifest.settings ? " · settings included" : " · no settings file";
  const carried =
    manifest.objects.length === 1
      ? `${manifest.objects.length} object carried`
      : `${manifest.objects.length} objects carried`;
  return `Backup from ${manifest.created_at}: ${countsLine(manifest.counts)} · ${carried}${settings}`;
}

/** The inspect result line (adds the container's own size when known). */
export function backupDoneLine(summary: BackupSummaryView): string {
  return `Backup written to ${summary.path} (${humanBytes(summary.size)}).`;
}

/** What a validated preview tells the user before they commit. */
export function previewLine(preview: RestorePreviewView): string {
  const settings = preview.restored_settings ? "settings will be restored" : "no settings in this backup";
  return `${manifestLine(preview.manifest)} — validated; ${settings}.`;
}

/** The post-commit sentence: restart + reconnection are always required
 *  (credentials are never part of a backup). */
export function restoreDoneLine(outcome: RestoreOutcomeView): string {
  const sources =
    outcome.sources_needing_reauth === 1
      ? "1 restored account"
      : `${outcome.sources_needing_reauth} restored accounts`;
  return (
    `Vault restored from backup. The replaced vault is kept as recovery snapshot ` +
    `${outcome.previous_slot}. Restart the app to open the restored vault` +
    (outcome.sources_needing_reauth > 0 ? `, then reconnect ${sources} (passwords are never restored).` : ".")
  );
}

/** One line per previous-vault slot. */
export function slotLine(slot: PreviousVaultSlot): string {
  return `${slot.created_at} · ${humanBytes(slot.total_bytes)} · ${slot.files} files`;
}

/** The recovery panel's headline: journal state + slot count. */
export function recoveryLine(status: RecoveryStatusView): string {
  if (status.previous.length === 0) {
    return "No previous-vault snapshots recorded yet (one is kept every time a restore replaces the vault).";
  }
  const slots = status.previous.length === 1 ? "snapshot" : "snapshots";
  let line = `${status.previous.length} previous-vault ${slots} kept:`;
  if (status.journal_state && status.journal_state !== "completed") {
    line += ` (a switch journal from ${status.journal_created_at ?? "an earlier session"} is unresolved — restart the app to recover it)`;
  }
  if (status.staged_preview) {
    line += " A validated restore is waiting to be committed.";
  }
  return line;
}

/** Whether the commit step is offered for the current flow state. */
export function canCommit(flow: RestoreFlow): boolean {
  return flow.step === "validated";
}

/** Render the restore flow panel: what is picked, what was validated,
 *  and the explicit actions for the current step. The callbacks are the
 *  main.ts wiring (validate, commit, dismiss); this function only draws. */
export function renderRestoreFlow(
  el: HTMLElement,
  flow: RestoreFlow,
  onValidate: (archive: string) => void,
  onCommit: (preview: RestorePreviewView) => void,
  onDismiss: () => void,
): void {
  el.replaceChildren();
  const line = (text: string, isError = false) => {
    const p = document.createElement("p");
    p.className = "meta";
    p.setAttribute("role", "status");
    if (isError) p.style.color = "var(--danger, #b00020)";
    p.textContent = text;
    el.append(p);
  };
  const button = (label: string, title: string, onClick: () => void) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.title = title;
    b.addEventListener("click", onClick);
    el.append(b);
    return b;
  };
  switch (flow.step) {
    case "idle":
      return;
    case "inspected": {
      line(manifestLine(flow.manifest));
      button("Validate for restore…", "Checksum every entry and validate the database in a staging area", () =>
        onValidate(flow.archive),
      );
      button("Dismiss", "Close this restore", onDismiss);
      return;
    }
    case "validating": {
      line("Validating the backup (re-checksumming every entry)…");
      return;
    }
    case "validated": {
      line(previewLine(flow.preview));
      line(
        "Restoring replaces the whole vault (messages, media, settings). The current vault is kept as a recovery snapshot; passwords are never restored.",
      );
      const commit = button("Restore now…", "Commit the validated restore through the recoverable switch", () =>
        onCommit(flow.preview),
      );
      commit.disabled = !canCommit(flow);
      button("Dismiss", "Keep the current vault; discard the staged restore", onDismiss);
      return;
    }
    case "committing": {
      line("Switching the vault in… (the app will ask for a restart when done)");
      return;
    }
    case "done": {
      line(restoreDoneLine(flow.outcome));
      return;
    }
    case "error": {
      line(flow.message, true);
      button("Dismiss", "Close this restore", onDismiss);
      return;
    }
  }
}

/** Render the previous-vault slots: one row per slot with its line and
 *  the explicit Return / Forget actions. */
export function renderRecoverySlots(
  el: HTMLElement,
  status: RecoveryStatusView,
  onReturn: (slotId: string) => void,
  onForget: (slotId: string) => void,
): void {
  el.replaceChildren();
  if (status.previous.length === 0) {
    const empty = document.createElement("p");
    empty.className = "meta";
    empty.textContent =
      "Nothing recorded — a snapshot is kept every time a restore or return replaces the vault.";
    el.append(empty);
    return;
  }
  for (const slot of status.previous) {
    const row = document.createElement("div");
    row.className = "row";
    const info = document.createElement("span");
    info.className = "meta";
    info.style.flex = "1";
    info.textContent = slotLine(slot);
    const back = document.createElement("button");
    back.type = "button";
    back.title = "Switch the active vault back to this snapshot (validated, journaled; restart required)";
    back.textContent = "Return to this vault";
    back.addEventListener("click", () => onReturn(slot.id));
    const forget = document.createElement("button");
    forget.type = "button";
    forget.title = "Delete this recovery snapshot for good (the active vault is never touched)";
    forget.textContent = "Forget";
    forget.addEventListener("click", () => onForget(slot.id));
    row.append(info, back, forget);
    el.append(row);
  }
}
