// Path: import an .mbox whose mails carry MIME attachments (a base64 PDF,
// a quoted-printable CSV) — M4.2 mail media: attachment bytes land
// hash-named in the app data media/ store, the archived message links
// them, the detail view lists them as available attachments (never "not
// in export"), and a re-import stores nothing new and dedups.
import path from "node:path";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const name = "mail-attachments";

const PDF_PAYLOAD = Buffer.from(
  "ZGVzaWduIG5vdGVzIHBheWxvYWQgdjEgLSBtYWlsY2hhdCBNNC4yIGF0dGFj" +
  "aG1lbnQgY29udGVudCwgZGV0ZXJtaW5pc3RpYyBieXRlcyBmb3IgdGhlIGUy" +
  "ZSBzdG9yZSBjaGVjaw==",
  "base64",
);
// MIME text parts are stored in canonical CRLF form with the trailing
// line break trimmed by the mail parser, so the expected bytes of the
// quoted-printable-decoded CSV use CRLF and no final newline.
const CSV_PAYLOAD = Buffer.from("quarter,amount\r\nQ1,3.140.00\r\nQ2,4.050.00", "utf8");
const sha = (buf) => createHash("sha256").update(buf).digest("hex");

export async function run(ctx) {
  const { withSession, assert, ui, sqlite, vaultDbPath } = ctx;
  const fixturesDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "fixtures");
  const mbox = path.join(fixturesDir, "mail-attachments.mbox");
  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    const status = await ui.importMbox(browser, mbox);
    assert.includes(status, "Imported 2 new, 0 already present", "import summary");
    const storeDir = path.join(dataDir, "com.acailic.mailchatvault", "media");
    const names = readdirSync(storeDir).sort();
    assert.equal(names.length, 2, "exactly two objects in the media store");
    const pdfStored = names.find((n) => n.endsWith(".pdf"));
    const csvStored = names.find((n) => n.endsWith(".csv"));
    const pdfSha = sha(PDF_PAYLOAD);
    const csvSha = sha(CSV_PAYLOAD);
    assert.ok(!!pdfStored && !!csvStored,
      "both objects stored under their extensions (got " + JSON.stringify(names) + ")");
    assert.equal(pdfStored, pdfSha + ".pdf", "PDF object is hash-named");
    assert.equal(csvStored, csvSha + ".csv", "CSV object is hash-named");
    assert.ok(Buffer.compare(readFileSync(path.join(storeDir, pdfStored)), PDF_PAYLOAD) === 0,
      "PDF bytes base64-decoded byte-identical into the store");
    assert.ok(Buffer.compare(readFileSync(path.join(storeDir, csvStored)), CSV_PAYLOAD) === 0,
      "CSV bytes quoted-printable-decoded byte-identical into the store");
    if (sqlite) {
      const db = vaultDbPath(dataDir);
      const rows = await sqlite.query(
        db,
        "SELECT external_id, raw_json FROM archive_messages WHERE external_id LIKE 'att-%' ORDER BY external_id",
      );
      assert.equal(rows.length, 2, "both mails archived");
      const m1 = JSON.parse(rows[0].raw_json);
      const m2 = JSON.parse(rows[1].raw_json);
      assert.equal(m1.media.length, 1, "m1 links one media entry");
      assert.equal(m1.media[0].name, "design-notes.pdf", "m1 entry name");
      assert.equal(m1.media[0].kind, "document", "m1 entry kind");
      assert.equal(m1.media[0].sha256, pdfSha, "m1 entry digest");
      assert.ok(!m1.media[0].missing, "m1 entry is not missing");
    }
    // --- detail view: both attachments render as available media entries
    for (const [query, file, kind, digest] of [
      ["agenda", "design-notes.pdf", "document", pdfSha],
      ["budget figures", "budget.csv", "text", csvSha],
    ]) {
      await ui.search(browser, query);
      await browser.execute(() =>
        document.querySelector("#results li.hit-item")?.click(),
      );
      await browser.waitUntil(
        () => browser.execute(() => document.querySelector("#detail.open") !== null),
        { timeoutMsg: "clicking the hit did not open the detail panel" },
      );
      const detailText = await browser.execute(
        () => document.querySelector("#detail")?.textContent ?? "",
      );
      assert.ok(detailText.includes(file), "detail shows the attachment name " + file);
      const items = await browser.execute(() =>
        Array.from(document.querySelectorAll("#detail .detail-media .media-item")).map(
          (el) => ({
            cls: el.className,
            kind: el.querySelector(".media-kind")?.textContent ?? "",
            name: el.querySelector(".media-name")?.textContent ?? "",
            meta: el.querySelector(".meta")?.textContent ?? "",
          }),
        ),
      );
      const hit = items.find((it) => it.name === file);
      assert.ok(!!hit, "media item present for " + file);
      assert.ok(!hit.cls.includes("unresolved"), file + " is not unresolved");
      assert.equal(hit.kind, kind, file + " kind badge");
      assert.ok(hit.meta.includes("media/" + digest.slice(0, 8)),
        file + " meta shows the store path (got " + JSON.stringify(hit.meta) + ")");
      await browser.execute(() => document.querySelector("#detail")?.classList.remove("open"));
    }
    // --- re-import is idempotent: nothing new stored, nothing re-linked
    const again = await ui.importMbox(browser, mbox);
    assert.includes(again, "Imported 0 new, 2 already present", "re-import dedups");
    assert.equal(readdirSync(storeDir).length, 2, "store unchanged after re-import");
    await browser.execute(() => location.reload());
    await ui.waitForApp(browser);
    await ui.search(browser, "agenda");
    await browser.execute(() => document.querySelector("#results li.hit-item")?.click());
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "detail did not reopen after reload" },
    );
    const afterReload = await browser.execute(() =>
      Array.from(document.querySelectorAll("#detail .detail-media .media-name")).map(
        (el) => el.textContent,
      ),
    );
    assert.ok(afterReload.includes("design-notes.pdf"),
      "media section persists after re-import and reload (got " + JSON.stringify(afterReload) + ")");

    // Reviewer-visible evidence of the mail-attachment detail view.
    const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR;
    if (evidenceDir) {
      try {
        const base64 = await browser.takeScreenshot();
        const { writeFileSync } = await import("node:fs");
        writeFileSync(path.join(evidenceDir, "mail-attachments-detail.png"),
          Buffer.from(base64, "base64"));
        process.stdout.write("screenshot: " + path.join(evidenceDir, "mail-attachments-detail.png") + "\n");
      } catch (e) {
        process.stdout.write("screenshot unavailable (" + String(e?.message ?? e) + ")\n");
      }
    }
  });
}
