// Path: the M2.3 IMAP coordinator surface an end user sees — the
// `imap_sync_all` coordinator round as a real v2 job through the live
// app, the M2.3 source surface (sources_list / source_set_excluded /
// source_failures) through the live IPC bridge, and the jobs panel
// label for the new job kind.
//
// The container has no TLS-trustable IMAP server the app's
// rustls/webpki-roots stack can trust (the same honest limitation the
// job-engine-parity spec records), so the *sync* paths of a round are
// covered by vault-core's mock-server suites; what this spec proves on
// the live product is:
//   - `imap_sync_all` is a real v2 job: submitted live, it runs, and
//     its typed RoundSummary result reaches the job record;
//   - the round partitions configured sources by exclusion — an
//     excluded source lands in `excluded` labels (skipped, no connect
//     attempt), a non-excluded source without stored credentials
//     lands as a `no_credentials` outcome, and no outcome lies;
//   - sources_list / source_set_excluded round-trip through the live
//     IPC with the flag persisted in the vault database;
//   - source_failures is the visible ledger (empty here);
//   - the jobs panel labels the round "IMAP sync (all sources)".

import { mkdirSync } from "node:fs";

export const name = "imap-coordinator";

const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR || null;

async function saveScreenshot(browser, filename) {
  if (!evidenceDir) {
    return null;
  }
  mkdirSync(evidenceDir, { recursive: true });
  const file = `${evidenceDir}/${filename}`;
  await browser.saveScreenshot(file);
  return file;
}

/** Invoke a Tauri command inside the live webview (real IPC, real app). */
async function ipc(browser, cmd, args = {}) {
  const settled = await ipcSettled(browser, cmd, args);
  if (settled.err !== undefined) {
    throw new Error(`${cmd} failed in webview: ${settled.err}`);
  }
  return settled.ok;
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
    { timeout: 20000, timeoutMsg: `${cmd} never settled in the webview` },
  );
  const settled = await browser.execute(() => window.__e2eInvoke);
  await browser.execute(() => {
    delete window.__e2eInvoke;
  });
  return settled;
}

/** Poll `job_get` until terminal; returns the final record. */
async function awaitJob(browser, id, { timeoutMs = 60000 } = {}) {
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

/** SQL to insert a disposable IMAP source row into the live vault DB
 * (the coordinator enumerates source_instances). The config mirrors
 * what `ensure_source_instance` stores for a Gmail-shaped source. */
function sourceRow(id, label, username, excluded) {
  const config = JSON.stringify({
    host: "127.0.0.1",
    port: 993,
    tls_mode: "implicit-tls",
    username,
    folders: ["INBOX"],
  });
  return `INSERT INTO source_instances (id, kind, label, connector_key, config_json, state, excluded)
          VALUES ('${id}', 'imap', '${label}', 'imap|127.0.0.1|993|implicit-tls|${username}', '${config}', 'connected', ${excluded ? 1 : 0})`;
}

/** Key-order-insensitive deep comparison (serde field order on the wire
 * differs from JS literal order; the shapes must still match exactly). */
function sameShape(a, b) {
  const norm = (v) =>
    Array.isArray(v)
      ? v.map(norm)
      : v && typeof v === "object"
        ? Object.fromEntries(
            Object.entries(v)
              .sort(([x], [y]) => (x < y ? -1 : 1))
              .map(([k, val]) => [k, norm(val)]),
          )
        : v;
  return JSON.stringify(norm(a)) === JSON.stringify(norm(b));
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
    // 1. The coordinator round as a real v2 job (no sources yet).
    // =============================================================
    const round1 = await ipc(browser, "job_submit", {
      kind: "imap_sync_all",
      params: {},
    });
    assert.equal(typeof round1.id, "number", "imap_sync_all submits as a v2 job");
    const r1 = await awaitJob(browser, round1.id);
    assert.equal(r1.kind, "imap_sync_all", "the job record carries the new kind");
    assert.equal(r1.state.state, "succeeded", "an empty round succeeds");
    assert.ok(
      sameShape(r1.state.result, { outcomes: [], excluded: [] }),
      "the RoundSummary wire shape over the live IPC",
    );

    // The jobs panel labels the round for an end user.
    await ui.openView(browser, "activity");
    const label = await ui.waitForText(browser, "#jobs-list", "IMAP sync (all sources)", {
      timeoutMsg: "the jobs panel never showed the imap_sync_all label",
    });
    assert.matches(label, /IMAP sync \(all sources\) — done/, "the panel entry shows the finished round");
    await browser.execute(() =>
      document.querySelector("#jobs-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "14-jobs-panel-imap-sync-all.png");

    // =============================================================
    // 2. Two disposable IMAP sources in the live vault DB: one
    //    excluded, one included with nothing in the keyring.
    // =============================================================
    await ctx.sqlite.query(db, sourceRow(
      "e2e-imap-excluded",
      "imap:mail.test:993:excluded@h.invalid",
      "excluded@h.invalid",
      true,
    ));
    await ctx.sqlite.query(db, sourceRow(
      "e2e-imap-credless",
      "imap:mail.test:993:credless@h.invalid",
      "credless@h.invalid",
      false,
    ));

    // =============================================================
    // 3. Round 2 partitions the two: excluded → `excluded` labels
    //    (skipped), included+credless → `no_credentials` outcome.
    // =============================================================
    const round2 = await ipc(browser, "job_submit", { kind: "imap_sync_all", params: {} });
    const r2 = await awaitJob(browser, round2.id);
    assert.equal(r2.state.state, "succeeded");
    assert.ok(
      sameShape(r2.state.result, {
        outcomes: [
          {
            instance_id: "e2e-imap-credless",
            label: "imap:mail.test:993:credless@h.invalid",
            status: { status: "no_credentials" },
          },
        ],
        excluded: ["imap:mail.test:993:excluded@h.invalid"],
      }),
      "excluded source skipped + credless source reported NoCredentials",
    );

    // Nothing connected anywhere: both attempt stamps stay empty
    // (no keyring password, no sync attempt), the excluded flag is
    // still set, and the archive stored no messages.
    const stamps = await ctx.sqlite.query(
      db,
      `SELECT id, last_attempt_at IS NULL AS never, excluded FROM source_instances
       WHERE id IN ('e2e-imap-excluded','e2e-imap-credless') ORDER BY id`,
    );
    assert.equal(stamps.length, 2, "both disposable sources present");
    assert.ok(stamps.every((s) => s.never === 1), "no source was connected to");
    assert.equal(
      stamps.find((s) => s.id === "e2e-imap-excluded").excluded,
      1,
      "exclusion flag still set in the DB",
    );

    // =============================================================
    // 4. sources_list / source_set_excluded / source_failures over
    //    the live IPC; the flag round-trips into the database.
    // =============================================================
    const listed = await ipc(browser, "sources_list");
    assert.equal(listed.length, 2, "sources_list returns every durable instance");
    const byId = Object.fromEntries(listed.map((s) => [s.id, s]));
    assert.equal(byId["e2e-imap-excluded"].excluded, true, "sources_list shows the exclusion flag");
    assert.equal(byId["e2e-imap-credless"].excluded, false);

    await ipc(browser, "source_set_excluded", { sourceId: "e2e-imap-excluded", excluded: false });
    const flagRow = await ctx.sqlite.query(
      db,
      "SELECT excluded FROM source_instances WHERE id = 'e2e-imap-excluded'",
    );
    assert.equal(flagRow[0].excluded, 0, "the re-include persisted to the database");

    // Round 3: both sources now included and credless → both reported
    // as no_credentials; the excluded list is empty.
    const round3 = await ipc(browser, "job_submit", { kind: "imap_sync_all", params: {} });
    const r3 = await awaitJob(browser, round3.id);
    assert.equal(r3.state.state, "succeeded");
    assert.equal(r3.state.result.excluded.length, 0, "nothing excluded anymore");
    assert.equal(r3.state.result.outcomes.length, 2);
    assert.ok(
      r3.state.result.outcomes.every((o) => o.status.status === "no_credentials"),
      "every included source without stored credentials reports no_credentials",
    );

    // The failed-item ledger is visible and empty (nothing fell).
    const failures = await ipc(browser, "source_failures", {});
    assert.ok(sameShape(failures, []), "the failed-item ledger is visible and empty");

    await browser.execute(() =>
      document.querySelector("#jobs-list")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "14-sources-after-reinclude.png");
    log("      coordinator surface driven live: round partition, no_credentials, exclusion round-trip, empty ledger");
  });
}
