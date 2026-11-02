// Export end-to-end: search-result and whole-chat exports to CSV, JSON
// Lines and MBOX through the running app's export_search / export_chat
// commands with the exact argument objects the UI button handlers build
// (the native GTK save dialog cannot be automated over WebDriver — same
// rationale as the media-extract and signal-import specs). Also drives
// the Export results… button in an empty archive, where the UI must
// refuse before any dialog, and proves the MBOX export is readable by
// the product's own importer (round-trip via import_mbox).
export const name = "export";

import { access, mkdir, readFile, rm } from "node:fs/promises";
import { writeFileSync } from "node:fs";
import path from "node:path";

/** Save a screenshot of the current viewport into the evidence dir when one
 * is provided (best-effort: some drivers lack the endpoint). */
async function saveScreenshot(browser, filename) {
  const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR;
  if (!evidenceDir) {
    return;
  }
  try {
    const base64 = await browser.takeScreenshot();
    writeFileSync(path.join(evidenceDir, filename), Buffer.from(base64, "base64"));
    process.stdout.write(`screenshot: ${path.join(evidenceDir, filename)}\n`);
  } catch (e) {
    process.stdout.write(`screenshot unavailable (${String(e?.message ?? e)})\n`);
  }
}

const workDir = "/tmp/mailchat-export-e2e";

/** Parse an RFC 4180 CSV string into records (arrays of fields). */
function parseCsv(text) {
  const records = [];
  let record = [];
  let field = "";
  let inQuotes = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inQuotes) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i++;
        } else {
          inQuotes = false;
        }
      } else {
        field += c;
      }
    } else if (c === '"') {
      inQuotes = true;
    } else if (c === ",") {
      record.push(field);
      field = "";
    } else if (c === "\r") {
      // part of the CRLF terminator, dropped
    } else if (c === "\n") {
      record.push(field);
      records.push(record);
      record = [];
      field = "";
    } else {
      field += c;
    }
  }
  if (field !== "" || record.length > 0) {
    record.push(field);
    records.push(record);
  }
  return records;
}

/** Invoke a Tauri command in the app and await { ok, value, error }. */
async function invoke(browser, command, args = {}) {
  try {
    const value = await browser.execute(
      async (cmd, payload) => window.__TAURI_INTERNALS__.invoke(cmd, payload),
      command,
      args,
    );
    return { ok: true, value };
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await rm(workDir, { recursive: true, force: true });
  await mkdir(workDir, { recursive: true });
  const out = (name) => path.join(workDir, name);

  // -- Session 1: mixed archive (demo emails + chats + mbox mails) --------
  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    await ui.seedDemo(browser);
    await ui.importMbox(browser, ui.archiveMbox);

    // Scenario: CSV export covers every row of a text search (all pages).
    const vaultStatus = await ui.search(browser, "vault");
    const mode = await ui.uiMode(browser);
    const vaultTotal = ui.totalFromStatus(vaultStatus, mode);
    await browser.execute(() =>
      document.querySelector("#export-controls")?.scrollIntoView({ block: "center" }),
    );
    await saveScreenshot(browser, "export-search-ui.png");

    // Open the detail view of the first hit: the detail header now carries
    // the ⤓ Export chat… button (rendered by src/render.ts).
    const firstRow = (await browser.$$("#results li.hit-item"))[0];
    await firstRow.click();
    await browser.$("#detail.open article.detail-card").waitForExist({
      timeout: 10_000,
      timeoutMsg: "detail view never opened",
    });
    assert.ok(
      await browser.$(".export-chat-btn").isExisting(),
      "detail header renders the Export chat… button",
    );
    await saveScreenshot(browser, "export-chat-detail.png");

    // The query object exactly as src/main.ts exportResults() builds it
    // from the executed search's state.
    const uiQuery = {
      text: "vault",
      sources: [],
      author: "",
      from_date: "",
      to_date: "",
      limit: 50,
      offset: 0,
    };

    const csv = await invoke(browser, "export_search", {
      query: uiQuery,
      format: "csv",
      path: out("vault.csv"),
    });
    assert.ok(csv.ok, `export_search csv succeeded: ${csv.error}`);
    assert.equal(csv.value.format, "csv", "summary format");
    assert.equal(csv.value.written, vaultTotal, "CSV rows == search total");
    assert.equal(csv.value.skipped, 0, "CSV skips nothing");

    const csvText = await readFile(out("vault.csv"), "utf8");
    const records = parseCsv(csvText);
    assert.equal(records.length, vaultTotal + 1, "header + N data records");
    assert.equal(
      records[0].join(","),
      "source,external_id,channel,chat_id,author,sent_at,body,raw_json",
      "CSV header",
    );
    for (const r of records.slice(1)) {
      assert.equal(r.length, 8, "each record has 8 fields");
      assert.ok(
        (r[6] + r[4] + r[7]).toLowerCase().includes("vault"),
        `row matches vault: ${(r[6] || r[4] || r[7]).slice(0, 40)}`,
      );
    }

    // Scenario: JSON Lines export of the same search is lossless — one
    // parseable Message per line, same rows in the same order as CSV.
    const json = await invoke(browser, "export_search", {
      query: uiQuery,
      format: "json",
      path: out("vault.jsonl"),
    });
    assert.ok(json.ok, `export_search json succeeded: ${json.error}`);
    assert.equal(json.value.written, vaultTotal, "JSONL rows == search total");
    const lines = (await readFile(out("vault.jsonl"), "utf8"))
      .split("\n")
      .filter((l) => l.trim() !== "");
    assert.equal(lines.length, vaultTotal, "one line per row");
    const messages = lines.map((l) => JSON.parse(l));
    for (let i = 0; i < vaultTotal; i++) {
      assert.equal(messages[i].body, records[i + 1][6], "same body as CSV row");
      assert.equal(messages[i].source, records[i + 1][0], "same source as CSV row");
    }
    for (const m of messages) {
      for (const key of [
        "source", "external_id", "channel", "chat_id",
        "author", "sent_at", "body", "raw_json",
      ]) {
        assert.ok(key in m, `message has ${key}`);
      }
    }

    // Scenario: formula-injection probe — chat bodies beginning with =, +,
    // - or @ export to CSV with an apostrophe prefix so the cells open as
    // inert text in Excel/LibreOffice; ordinary bodies stay unchanged.
    const probeTxt = path.join(workDir, "formula-probe.txt");
    writeFileSync(
      probeTxt,
      [
        "[1.11.2026., 09:00:00] Formula Probe: =cmd|' /C calc'!A0 formula injection probe",
        "[1.11.2026., 09:00:01] Formula Probe: +SUM(A1) plus-prefix probe",
        "[1.11.2026., 09:00:02] Formula Probe: -2+3 minus-prefix probe",
        "[1.11.2026., 09:00:03] Formula Probe: @everyone mention probe",
        "[1.11.2026., 09:00:04] Formula Probe: plain probe body without prefix",
      ].join("\n") + "\n",
    );
    const probeImport = await invoke(browser, "import_chat_export", { path: probeTxt });
    assert.ok(probeImport.ok, `formula probe import succeeded: ${probeImport.error}`);
    const probeCsv = await invoke(browser, "export_search", {
      query: { ...uiQuery, text: "probe", sources: ["whatsapp"] },
      format: "csv",
      path: out("formula-probe.csv"),
    });
    assert.ok(probeCsv.ok, `formula probe export succeeded: ${probeCsv.error}`);
    assert.equal(probeCsv.value.written, 5, "all five probe rows exported");
    const probeRecords = parseCsv(await readFile(out("formula-probe.csv"), "utf8"));
    const probeBodies = probeRecords.slice(1).filter((r) => r[3] === "formula-probe").map((r) => r[6]);
    assert.equal(probeBodies.length, 5, "five probe body cells found");
    // Search order is relevance-ranked, not insertion order — compare sets.
    assert.equal(
      JSON.stringify(probeBodies.sort()),
      JSON.stringify([
        "'=cmd|' /C calc'!A0 formula injection probe",
        "'+SUM(A1) plus-prefix probe",
        "'-2+3 minus-prefix probe",
        "'@everyone mention probe",
        "plain probe body without prefix",
      ].sort()),
      "every formula-prefix cell neutralized, plain body untouched",
    );

    // Scenario: MBOX export over a browse-all of the mixed archive —
    // email rows written, chat rows skipped and reported in the summary.
    const allCsv = await invoke(browser, "export_search", {
      query: { ...uiQuery, text: "" },
      format: "csv",
      path: out("all.csv"),
    });
    assert.ok(allCsv.ok, `browse-all csv succeeded: ${allCsv.error}`);
    const browseTotal = allCsv.value.written;

    const mbox = await invoke(browser, "export_search", {
      query: { ...uiQuery, text: "" },
      format: "mbox",
      path: out("all.mbox"),
    });
    assert.ok(mbox.ok, `export_search mbox succeeded: ${mbox.error}`);
    // Count email vs non-email rows from the CSV of the same selection.
    const allRecords = parseCsv(await readFile(out("all.csv"), "utf8"));
    const emailCount = allRecords.slice(1).filter((r) => r[2] === "email").length;
    const chatCount = browseTotal - emailCount;
    assert.equal(mbox.value.written, emailCount, "MBOX written == email rows");
    assert.equal(mbox.value.skipped, chatCount, "MBOX skipped == chat rows");

    const mboxText = await readFile(out("all.mbox"), "utf8");
    const fromLines = mboxText.split("\n").filter((l) => l.startsWith("From "));
    assert.equal(fromLines.length, emailCount, "one From separator per email row");
    for (const sep of fromLines) {
      assert.ok(!sep.startsWith("From  "), "separator lines are well-formed");
    }

    // Scenario: MBOX round-trip — the exported mbox imports cleanly through
    // the product's own importer (proves the format is readable again).
    const roundtrip = await invoke(browser, "import_mbox", {
      path: out("all.mbox"),
    });
    assert.ok(roundtrip.ok, `re-import of exported mbox succeeded: ${roundtrip.error}`);
    assert.equal(roundtrip.value.failed, 0, "no failed rows in mbox round-trip");
    assert.ok(
      roundtrip.value.inserted + roundtrip.value.skipped >= emailCount,
      `re-import accounted for every email (${roundtrip.value.inserted} new, ${roundtrip.value.skipped} deduped)`,
    );

    // Scenario: whole-chat export — the thread of a chat message exports
    // completely, oldest → newest.
    const chatSearch = await invoke(browser, "search_messages", {
      query: { ...uiQuery, text: "deploy", limit: 10 },
    });
    assert.ok(chatSearch.ok, `search_messages succeeded: ${chatSearch.error}`);
    const chatHit = chatSearch.value.hits.find((h) => h.message.chat_id !== "");
    assert.ok(chatHit, "found a threaded chat hit for 'deploy'");
    const chatId = chatHit.message.chat_id;
    const chatExport = await invoke(browser, "export_chat", {
      id: chatHit.message.id,
      format: "csv",
      path: out("thread.csv"),
    });
    assert.ok(chatExport.ok, `export_chat csv succeeded: ${chatExport.error}`);
    assert.ok(chatExport.value !== null, "export_chat found the message");
    const threadRecords = parseCsv(await readFile(out("thread.csv"), "utf8"));
    const threadRows = threadRecords.slice(1);
    assert.ok(
      threadRows.length >= 2,
      `thread export has the whole thread (${threadRows.length} rows)`,
    );
    for (const r of threadRows) {
      assert.equal(r[3], chatId, "every exported row belongs to the thread");
    }
    const sentTimes = threadRows.map((r) => r[5]);
    const sorted = [...sentTimes].sort();
    assert.equal(
      JSON.stringify(sorted),
      JSON.stringify(sentTimes),
      "thread rows are oldest → newest",
    );

    // Scenario: chat thread to MBOX — chat rows are not email, so the
    // whole thread is skipped and reported.
    const chatMbox = await invoke(browser, "export_chat", {
      id: chatHit.message.id,
      format: "mbox",
      path: out("thread.mbox"),
    });
    assert.ok(chatMbox.ok, `export_chat mbox succeeded: ${chatMbox.error}`);
    assert.equal(chatMbox.value.written, 0, "no email rows in a chat thread");
    assert.equal(chatMbox.value.skipped, threadRows.length, "every chat row skipped");

    // Scenario: unknown id — export_chat returns null (message gone).
    const missing = await invoke(browser, "export_chat", {
      id: 999999,
      format: "csv",
      path: out("missing.csv"),
    });
    assert.ok(missing.ok, `export_chat unknown id succeeded: ${missing.error}`);
    assert.equal(missing.value, null, "unknown id reports None");
    let missingFileExists = true;
    try {
      await access(out("missing.csv"));
    } catch {
      missingFileExists = false;
    }
    assert.ok(
      !missingFileExists,
      "unknown id leaves no file at the chosen path",
    );

    // Scenario: unknown format is a caller error, not a silent file.
    const badFormat = await invoke(browser, "export_search", {
      query: uiQuery,
      format: "pdf",
      path: out("bad.pdf"),
    });
    assert.ok(!badFormat.ok, "unknown format is rejected");
    assert.matches(badFormat.error, /unknown format/, "error names the format problem");
  });

  // -- Session 2: empty archive — the Export results… button must refuse
  // before any dialog when there is nothing to export (real UI click).
  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    assert.equal(await ui.waitForApp(browser), "empty archive", "fresh install stats label");

    const button = await browser.$("#export-btn");
    assert.ok(await button.isExisting(), "export button exists");
    await button.click();
    const status = await ui.waitForText(
      browser,
      "#export-status",
      (text) => text.includes("Nothing to export"),
      { timeoutMsg: "empty-archive export never produced the guard message" },
    );
    assert.includes(status, "Nothing to export");

    // With a zero-result search (filters can't match), same guard.
    const miss = await ui.search(browser, "quartzxyz");
    assert.matches(miss, /0 results|0 of/, "zero-result search ran");
    await button.click();
    const status2 = await ui.waitForText(
      browser,
      "#export-status",
      (text) => text.includes("Nothing to export"),
      { timeoutMsg: "zero-result export never produced the guard message" },
    );
    assert.includes(status2, "Nothing to export");
  });

}
