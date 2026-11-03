import { describe, expect, it } from "vitest";
import {
  INTERVAL_CHOICES,
  autoSyncStatusText,
  formatCountdown,
  isIntervalChoice,
} from "./schedview";
import type { SyncScheduleView } from "./types";

function view(overrides: Partial<SyncScheduleView> = {}): SyncScheduleView {
  return {
    enabled: false,
    interval_minutes: 15,
    active_job: null,
    next_due_in_secs: null,
    rounds_submitted: 0,
    ...overrides,
  };
}

describe("formatCountdown", () => {
  it("renders seconds close-in, minutes mid-range, hours far out", () => {
    expect(formatCountdown(45)).toBe("~45s");
    expect(formatCountdown(90)).toBe("~90s");
    expect(formatCountdown(91)).toBe("~2 min");
    expect(formatCountdown(1800)).toBe("~30 min");
    expect(formatCountdown(90 * 60)).toBe("~2 h");
  });
});

describe("autoSyncStatusText", () => {
  it("off: names the manual paths", () => {
    const text = autoSyncStatusText(view());
    expect(text).toContain("Auto-sync off");
    expect(text).toContain("Sync");
  });

  it("on with a countdown", () => {
    const text = autoSyncStatusText(view({ enabled: true, next_due_in_secs: 870 }));
    expect(text).toContain("Auto-sync on");
    expect(text).toContain("~15 min");
  });

  it("on without a countdown (armed but unpublished)", () => {
    const text = autoSyncStatusText(view({ enabled: true }));
    expect(text).toContain("next round scheduled");
  });

  it("a running round wins over the countdown", () => {
    const text = autoSyncStatusText(
      view({ enabled: true, active_job: 12, next_due_in_secs: 300 }),
    );
    expect(text).toContain("job 12");
    expect(text).not.toContain("~5 min");
  });
});

describe("interval choices", () => {
  it("offers 15 minutes among a small set, defaulting there", () => {
    expect(INTERVAL_CHOICES).toContain(15);
    expect(isIntervalChoice(15)).toBe(true);
    expect(isIntervalChoice(5)).toBe(true);
    expect(isIntervalChoice(60)).toBe(true);
    expect(isIntervalChoice(7)).toBe(false);
    expect(isIntervalChoice(1440)).toBe(false);
  });
});
