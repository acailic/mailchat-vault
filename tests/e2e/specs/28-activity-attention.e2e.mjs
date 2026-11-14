// Path: the Activity nav attention flag, driven live at the product
// surface: a background job that fails while the user is on another view
// must flag the Activity nav link; opening Activity acknowledges it (the
// flag clears); navigating away must NOT reflag the acknowledged failure;
// a boot poll that recovers a previously missed failure flags again; and
// a NEW failure after acknowledgment reflags with a higher seq. The
// failure is produced by the real job engine: an imap_sync job against a
// closed local port (fast connect refusal, the same failure class the
// job-engine-parity spec drives).
//
// Session-scoped acknowledgment across an app restart (the tracker and
// ack reset, so a previously acknowledged failure reflags after a webview
// reload) is a recorded deliberate tradeoff, not asserted here as a
// defect — the reload step below asserts the CURRENT behavior (the boot
// poll's recovered-failure path flags attention), which is the intended
// reconciliation contract from this change.

import net from "node:net";
import { mkdirSync } from "node:fs";

export const name = "activity-attention";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR ?? null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) return null;
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

/** Reserve a free TCP port (nothing listens; the imap_sync job fast-fails). */
function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

function imapParams(port) {
  return {
    kind: "imap_sync",
    params: {
      host: "127.0.0.1",
      port,
      username: "attention@h.invalid",
      password: "attention-e2e-pw",
    },
  };
}

/** The attention flag's `hidden` attribute, read via execute (live-region
 * getText quirks; hidden is an attribute read, not text). */
async function attentionHidden(browser) {
  return browser.execute(
    () => document.querySelector("#activity-attention")?.hidden ?? null,
  );
}

/** Wait for the flag's hidden state to settle on the wanted value. */
async function waitForAttention(browser, wanted, msg) {
  await browser.waitUntil(
    async () => (await attentionHidden(browser)) === wanted,
    { timeout: 30_000, timeoutMsg: msg },
  );
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // --- 1. clean boot: no failures, no flag ---------------------------
    assert.ok(
      await attentionHidden(browser),
      "a clean boot shows no attention flag",
    );
    await saveScreenshot(browser, "28-clean-boot.png");

    // --- 2. failure while on another view flags the nav link -----------
    await ui.openView(browser, "workspace");
    const port1 = await freePort();
    const job1 = await ui.invokeCommand(browser, "job_submit", imapParams(port1));
    assert.equal(typeof job1.id, "number", "imap_sync submit returns a job handle");
    await waitForAttention(
      browser,
      false,
      "the attention flag never appeared for the first failed sync",
    );
    const rec1 = await ui.invokeCommand(browser, "job_get", { id: job1.id });
    assert.equal(rec1.state.state, "failed", "the sync job failed (closed port)");
    await saveScreenshot(browser, "28-flag-on-failure.png");

    // --- 3. opening Activity acknowledges: the flag clears -------------
    await ui.openView(browser, "activity");
    await waitForAttention(browser, true, "opening Activity never cleared the flag");
    await saveScreenshot(browser, "28-acked-in-activity.png");

    // --- 4. leaving Activity does not reflag the acknowledged failure ---
    await ui.openView(browser, "workspace");
    await new Promise((r) => setTimeout(r, 1_000)); // let any wrong reflag fire
    assert.ok(
      await attentionHidden(browser),
      "the acknowledged failure was reflagged after leaving Activity",
    );

    // --- 5. boot recovery: after a webview reload the boot poll sees the
    //        failed record and flags attention again (the recovered-
    //        failure path this change added; the re-show after a session
    //        reset is the recorded intended behavior) --------------------
    await browser.refresh();
    await ui.waitForApp(browser);
    await waitForAttention(
      browser,
      false,
      "the boot poll never recovered the missed failure into the flag",
    );
    await saveScreenshot(browser, "28-recovered-after-reload.png");

    // --- 6. a NEW failure reflags attention (seq above the ack) --------
    await ui.openView(browser, "workspace");
    const port2 = await freePort();
    const job2 = await ui.invokeCommand(browser, "job_submit", imapParams(port2));
    await waitForAttention(
      browser,
      false,
      "a second failure after acknowledgment never reflagged",
    );
    const rec2 = await ui.invokeCommand(browser, "job_get", { id: job2.id });
    assert.equal(rec2.state.state, "failed", "the second sync job also failed");
    await saveScreenshot(browser, "28-reflagged-new-failure.png");
  });
}
