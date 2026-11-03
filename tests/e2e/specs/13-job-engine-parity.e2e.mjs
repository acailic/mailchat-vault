// Path: the v2 background-job engine and the v2 imap_sync job, driven
// live against the running app under xvfb: the real IPC layer
// (`window.__TAURI_INTERNALS__.invoke`, the same seam the discord spec
// already uses), the real runner, the real keyring seam, the real vault
// database.
//
// Covered here, all against the live product:
//   - IMAP keyring parity: an entered password is saved and later reused
//     (empty-password resubmit resolves the stored credential); an empty
//     password with nothing stored fails with the documented error (which
//     also proves v2 parse accepts an empty password); a whitespace-padded
//     username and its trimmed form map to the SAME durable source
//     instance (one row in SQLite, not two).
//   - v2 imap_sync job through to its typed job result (the container has
//     no IMAP in-container server the app's rustls/webpki-roots TLS stack
//     can trust, so the driven result is the typed network failure — the
//     success path stays covered by vault-core's mock-server suites;
//     recorded honestly in the run's scenario list).
//   - Engine invariants: incremental idempotent embedding (re-run embeds
//     0 new, remaining 0); the flat JobEvent wire (event.type at the top
//     level — the round-1 regression) and monotonic recoverable event
//     seqs (a `job_events_since(cursor)` returns exactly the gap);
//     bounded job-queue backpressure (the 65th in-flight submission is
//     rejected with the typed queue-full error — the production semantic
//     of `try_submit_job` used by `job_submit`); cooperative cancellation
//     (a queued job cancelled before start ends `cancelled` without
//     running; a running embed job cancelled between batches ends
//     `cancelled`, not `succeeded`).

import { createServer } from "node:net";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

export const name = "job-engine-parity";

const HOST = "127.0.0.1";
/** Bulk-archive size for the running-cancel scenario (batches of 500). */
const BIG_ARCHIVE = 30_000;
/** How long the silent TCP listener parks the runner before release. */
const HANG_MS = 6_000;

/** Invoke a Tauri command inside the live webview (real IPC, real app).
 * Resolves with the command's value; rejects with its error string. */
async function ipc(browser, cmd, args = {}) {
  const settled = await ipcSettled(browser, cmd, args);
  if (settled.err !== undefined) {
    throw new Error(`${cmd} failed in webview: ${settled.err}`);
  }
  return settled.ok;
}

/** Same, but never throws: the settled envelope `{ok} | {err}` comes back
 * either way — for asserting the documented rejection texts. */
async function ipcTry(browser, cmd, args = {}) {
  return ipcSettled(browser, cmd, args);
}

async function ipcSettled(browser, cmd, args) {
  await browser.execute(
    (command, payload) => {
      window.__e2eInvoke = null;
      window.__TAURI_INTERNALS__.invoke(command, payload)
        .then((value) => {
          window.__e2eInvoke = { ok: value };
        })
        .catch((error) => {
          window.__e2eInvoke = { err: String(error) };
        });
    },
    cmd,
    args,
  );
  await browser.waitUntil(
    async () => (await browser.execute(() => window.__e2eInvoke)) !== null,
    { timeout: 20_000, timeoutMsg: `${cmd} never settled in the webview` },
  );
  const settled = await browser.execute(() => window.__e2eInvoke);
  await browser.execute(() => {
    delete window.__e2eInvoke;
  });
  return settled;
}

/** Poll `job_get` until terminal; returns the final record. */
async function awaitJob(browser, id, { timeoutMs = 120_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rec = await ipc(browser, "job_get", { id });
    const state = rec?.state?.state;
    if (state === "succeeded" || state === "failed" || state === "cancelled") {
      return rec;
    }
    await new Promise((resolve) => setTimeout(resolve, 150));
  }
  throw new Error(`job ${id} never reached a terminal state within ${timeoutMs}ms`);
}

/** Poll `job_get` until `predicate(rec)` holds; returns the record. */
async function pollJob(browser, id, predicate, { timeoutMs = 60_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const rec = await ipc(browser, "job_get", { id });
    if (rec && predicate(rec)) {
      return rec;
    }
    if (rec?.state?.state !== "running" && rec?.state?.state !== "queued" && rec?.state?.state !== undefined) {
      // Terminal before the predicate ever held (e.g. fast-failed job).
      return rec;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`job ${id}: predicate never held within ${timeoutMs}ms`);
}

/** Wait until every current job record is terminal (drained queue). */
async function awaitAllTerminal(browser, { timeoutMs = 180_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const jobs = await ipc(browser, "job_list");
    if (jobs.every((r) => ["succeeded", "failed", "cancelled"].includes(r.state.state))) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`queue never drained within ${timeoutMs}ms`);
}

/** Reserve a free TCP port (kernel-assigned, immediately released). */
async function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
    server.on("error", reject);
  });
}

/** An `imap_sync` job against a port with no listener fails fast with the
 * typed network error, AFTER keyring resolution and instance registration
 * — exactly the seam the parity scenarios observe. */
function imapParams(port, username, password) {
  return {
    kind: "imap_sync",
    params: { host: HOST, port, username, password },
  };
}

/** An N-message mbox file for the big-archive embed-cancel scenario. */
function writeBigMbox(n) {
  const dir = mkdtempSync(path.join(tmpdir(), "mc-e2e-big-"));
  const file = path.join(dir, "big.mbox");
  const chunk = [
    "From aaa@bbb.example Mon Jan 01 00:00:00 2024",
    "From: Big Archive <big@example.invalid>",
    "To: vault@example.invalid",
    "Subject: bulk message %N%",
    "Date: Mon, 01 Jan 2024 10:00:00 +0000",
    "Message-ID: <bulk-%N%@example.invalid>",
    "",
    "Body text for bulk message %N%, embedding checkpoint fuel.",
    "",
  ].join("\n");
  let out = "";
  for (let i = 0; i < n; i++) {
    out += chunk.replaceAll("%N%", String(i)) + "\n";
  }
  writeFileSync(file, out);
  return file;
}

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath, log } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    if (!sqlite) {
      throw new Error("sqlite3 CLI unavailable — this spec needs direct DB asserts");
    }
    const db = vaultDbPath(dataDir);

    // =============================================================
    // 1. IMAP keyring parity, driven live through job_submit
    // =============================================================
    const parityPort = await freePort(); // no listener: jobs fast-fail at connect

    // 1a. Empty password, NOTHING stored: the documented parity error.
    //     Reaching keyring resolution at all proves v2 parse accepted the
    //     empty password (a parse rejection would say "params.password is
    //     required").
    const nobody = await ipcTry(browser, "job_submit", imapParams(parityPort, "nobody@h.invalid", ""));
    assert.matches(nobody.err, /no password stored for this account — enter one/);

    // 1b. Entered password is saved: submit resolves and runs (the job
    //     itself fast-fails at the closed port — see its record below).
    const entered = await ipc(browser, "job_submit", imapParams(parityPort, " a@h.invalid ", "parity-pw-1"));
    assert.equal(typeof entered.id, "number", "entered-password submit returns a handle");

    // 1c. Empty password now resolves the stored credential — same
    //     account, TRIMMED username, no password. If the parse dropped
    //     the trim, the lookup key would differ and this submit would
    //     fail with the parity error instead.
    const reused = await ipc(browser, "job_submit", imapParams(parityPort, "a@h.invalid", ""));
    assert.equal(typeof reused.id, "number", "empty-password resubmit resolves the stored credential");

    const rec1 = await awaitJob(browser, entered.id);
    assert.equal(rec1.kind, "imap_sync", "v2 imap_sync job kind on the record");
    assert.equal(rec1.state.state, "failed", "job fast-fails against the closed port");
    // A refused TCP connect surfaces as the typed io error (vault-core's
    // From<Error> maps socket errors to JobError::Io); TLS-layer failures
    // would be network. Both are the connect phase.
    assert.ok(
      rec1.state.error.kind === "io" || rec1.state.error.kind === "network",
      `typed connect-phase failure on the record (got ${rec1.state.error.kind})`,
    );
    assert.matches(rec1.state.error.message, /refused|closed|reset|handshake|EOF/i, "connect-phase failure");

    // 1d. One durable source instance despite the padded username: both
    //     submits must have mapped to the same connector identity.
    const inst = await ctx.sqlite.query(
      db,
      `SELECT connector_key FROM source_instances
       WHERE connector_key LIKE 'imap|127.0.0.1|${parityPort}|%'`,
    );
    assert.equal(inst.length, 1, "padded + trimmed username = ONE durable source instance");
    assert.equal(
      inst[0].connector_key,
      `imap|127.0.0.1|${parityPort}|implicit-tls|a@h.invalid`,
      "instance key holds the trimmed username",
    );
    log(`      parity: instance key ${inst[0].connector_key}, typed failure: ${rec1.state.error.message}`);

    // =============================================================
    // 2. v2 imap_sync job through to its typed job result (driven live
    //    above: kind, keyring resolution, instance registration, runner,
    //    typed result). The SUCCESS path needs a TLS-trustable IMAP
    //    server the container cannot provide — recorded honestly in the
    //    run's scenario list; the sync engine itself is covered by
    //    vault-core's mock-server suites.
    // =============================================================

    // =============================================================
    // 3. Engine invariants
    // =============================================================
    await ui.importMbox(browser, ui.archiveMbox);

    // 3a. Incremental idempotent embedding: first run embeds the archive,
    //     second run embeds 0 new with remaining 0.
    const e1 = await ipc(browser, "job_submit", { kind: "embed_messages", params: {} });
    const r1 = await awaitJob(browser, e1.id);
    assert.equal(r1.state.state, "succeeded", "first embed run succeeds");
    assert.equal(r1.state.result.embedded, 8, "first run embeds the 8 fixture messages");
    assert.equal(r1.state.result.remaining, 0, "first run leaves nothing pending");

    const e2 = await ipc(browser, "job_submit", { kind: "embed_messages", params: {} });
    const r2 = await awaitJob(browser, e2.id);
    assert.equal(r2.state.state, "succeeded", "second embed run succeeds");
    assert.equal(r2.state.result.embedded, 0, "re-run embeds 0 new");
    assert.equal(r2.state.result.remaining, 0, "re-run reports remaining 0");

    // 3b. Flat JobEvent wire + monotonic recoverable event seqs.
    const cursor = (await ipc(browser, "job_events_since", { seq: 0 })).latest_seq;
    const e3 = await ipc(browser, "job_submit", { kind: "embed_messages", params: {} });
    await awaitJob(browser, e3.id);

    const page = await ipc(browser, "job_events_since", { seq: cursor });
    assert.equal(page.trimmed, false, "ring holds the whole window");
    let prev = cursor;
    for (const ev of page.events) {
      assert.ok(ev.seq > prev, "event seqs ascend above the cursor");
      prev = ev.seq;
      assert.equal(typeof ev.type, "string", "event.type is the flat top-level wire field");
      assert.equal(ev.job, e3.id, "the gap holds exactly the new job's events");
    }
    assert.equal(page.events[0].type, "submitted", "first gap event is the submitted event");
    assert.equal(page.events[0].kind, "embed_messages", "submitted event carries the job kind");
    assert.equal(
      page.events[page.events.length - 1].type,
      "succeeded",
      "last gap event is the terminal event",
    );

    // A later poll returns exactly nothing new, with an unchanged cursor.
    const page2 = await ipc(browser, "job_events_since", { seq: page.latest_seq });
    assert.equal(page2.events.length, 0, "cursor returns exactly the gap and nothing more");
    assert.equal(page2.latest_seq, page.latest_seq, "latest_seq monotonic and stable");

    // =============================================================
    // 3c. Bounded job-queue backpressure + queued-job cooperative cancel:
    //     park the runner on a silent TCP listener, fill the 64-slot
    //     queue, expect the typed queue-full rejection, cancel a queued
    //     job, then release the listener — the cancelled job must end
    //     `cancelled` (the runner checks the flag at the job's pre-start
    //     safe point) without ever running.
    // =============================================================
    const hangPort = await freePort();
    const sockets = [];
    const hang = createServer((socket) => sockets.push(socket));
    await new Promise((resolve, reject) => {
      hang.listen(hangPort, HOST, resolve);
      hang.on("error", reject);
    });

    const hung = await ipc(browser, "job_submit", imapParams(hangPort, "hang@h.invalid", "hang-pw"));
    await pollJob(browser, hung.id, (rec) => rec.state.state === "running");

    const target = await ipc(browser, "job_submit", { kind: "seed_demo", params: {} });
    for (let i = 0; i < 63; i++) {
      await ipc(browser, "job_submit", imapParams(parityPort, "a@h.invalid", ""));
    }
    const overflow = await ipcTry(browser, "job_submit", { kind: "seed_demo", params: {} });
    assert.matches(overflow.err, /job queue is full/, "65th in-flight submission hits the typed queue-full error");

    // Cancel the queued target while the runner is parked.
    assert.equal(
      await ipc(browser, "job_cancel", { id: target.id }),
      true,
      "queued job accepted the cancel request",
    );

    // Release the parked connection: EOF → typed network failure.
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => hang.close(resolve));

    const trec = await awaitJob(browser, target.id);
    assert.equal(trec.state.state, "cancelled", "cancelled-while-queued job never runs — ends cancelled");
    const hungDone = await awaitJob(browser, hung.id);
    assert.equal(hungDone.state.state, "failed", "released parked job fails typed");
    assert.ok(
      hungDone.state.error.kind === "io" || hungDone.state.error.kind === "network",
      `parked job's failure is typed connect-phase (got ${hungDone.state.error.kind})`,
    );

    // =============================================================
    // 3d. Cancel a RUNNING job: a big archive gives the embed job many
    //     batches; the cancel lands between batches, and the job must end
    //     `cancelled` — never `succeeded`.
    // =============================================================
    await awaitAllTerminal(browser);
    const mbox = writeBigMbox(BIG_ARCHIVE);
    const importJob = await ipc(browser, "job_submit", {
      kind: "import_mbox",
      params: { path: mbox },
    });
    const imported = await awaitJob(browser, importJob.id, { timeoutMs: 300_000 });
    assert.equal(imported.state.state, "succeeded", "big mbox imports via the v2 import_mbox job");
    assert.equal(imported.state.result.inserted, BIG_ARCHIVE, "all bulk messages inserted");

    const big = await ipc(browser, "job_submit", { kind: "embed_messages", params: {} });
    const runningRec = await pollJob(browser, big.id, (rec) => rec.state.state === "running");
    assert.ok(runningRec, "big embed job starts running");
    assert.equal(
      await ipc(browser, "job_cancel", { id: big.id }),
      true,
      "running job accepted the cancel request",
    );
    const bigRec = await awaitJob(browser, big.id, { timeoutMs: 180_000 });
    assert.equal(bigRec.state.state, "cancelled", "running embed job ends cancelled (not succeeded)");

    // The cancelled job's event timeline carries started → … → cancelled.
    const eventsAll = await ipc(browser, "job_events_since", { seq: 0 });
    const timeline = eventsAll.events.filter((ev) => ev.job === big.id).map((ev) => ev.type);
    assert.ok(timeline.includes("started"), "started event in the timeline");
    assert.ok(timeline.includes("cancelled"), "cancelled event in the timeline");
  });
}
