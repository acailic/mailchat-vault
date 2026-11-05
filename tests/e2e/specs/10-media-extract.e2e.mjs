// Media extraction end-to-end: import a Telegram export (real fixture
// files) through the running app's import_chat_export command with the
// same wire shape the "Import chat export..." button sends, verify the
// content-addressed store on disk, the media section in the detail view,
// the extraction-off path, one re-import dedup, and the WhatsApp
// stem-matching guard (plan.pdf.bak must not answer for a missing
// plan.pdf, while IMG-1.jpg still matches IMG-1.jpeg).
export const name = "media-extract";

import { copyFile, mkdir, writeFile, readFile, readdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";
import { fixturesDir } from "../harness/ui.mjs";

// Real fixture files committed with the vault-core integration tests.
const mediaFixtures = path.resolve(
  fixturesDir,
  "..",
  "..",
  "..",
  "crates",
  "vault-core",
  "tests",
  "fixtures",
  "media",
);

const sha256Hex = async (p) => createHash("sha256").update(await readFile(p)).digest("hex");

// Invoke a Tauri command in the app and await { ok, value, error }.
// Same shape and rationale as the signal-import spec: the native GTK file
// chooser cannot be automated over WebDriver, so the command is invoked
// with the exact argument object the button handler builds.
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

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);
    const storeDir = path.join(dataDir, "com.acailic.mailchatvault", "media");

    // --- disposable Telegram export with real media -----------------------
    const exportDir = path.join(dataDir, "tg-export");
    await mkdir(path.join(exportDir, "photos"), { recursive: true });
    await mkdir(path.join(exportDir, "files"), { recursive: true });
    await mkdir(path.join(exportDir, "voice_messages"), { recursive: true });
    await mkdir(path.join(exportDir, "video_files"), { recursive: true });
    const photo = path.join(mediaFixtures, "photo.jpg");
    const plan = path.join(mediaFixtures, "plan.pdf");
    const beep = path.join(mediaFixtures, "beep.wav");
    await copyFile(photo, path.join(exportDir, "photos/photo_102@01-11-2023_11-02-00.jpg"));
    await copyFile(plan, path.join(exportDir, "files/plan.pdf"));
    await copyFile(beep, path.join(exportDir, "voice_messages/audio_104@01-11-2023.ogg"));
    await writeFile(
      path.join(exportDir, "result.json"),
      JSON.stringify({
        name: "Arhiva ekipa",
        type: "private_group",
        id: 123456789,
        messages: [
          { id: 100, type: "message", date_unixtime: "1698832800", from: "Marko", text: "zdravo ekipa" },
          { id: 102, type: "message", date_unixtime: "1698832920", from: "Ana",
            photo: "photos/photo_102@01-11-2023_11-02-00.jpg", width: 800, height: 600, text: "" },
          { id: 103, type: "message", date_unixtime: "1698832980", from: "Marko", file: "files/plan.pdf", text: "" },
          { id: 104, type: "message", date_unixtime: "1698833040", from: "Jelena",
            media_type: "voice_message", file: "voice_messages/audio_104@01-11-2023.ogg", text: "" },
          { id: 105, type: "message", date_unixtime: "1698833100", from: "Jelena",
            media_type: "video_file",
            photo: "photos/photo_102@01-11-2023_11-02-00.jpg",
            file: "video_files/clip.mp4", text: "" },
        ],
      }),
    );
    await writeFile(path.join(exportDir, "video_files/clip.mp4"), Buffer.from("fake-video-bytes"));

    // --- import with the wire shape the UI button sends -------------------
    const outcome = await invoke(browser, "import_chat_export", {
      path: exportDir,
      extractMedia: true,
    });
    assert.ok(outcome.ok, `telegram import succeeded: ${outcome.error ?? ""}`);
    const s = outcome.value.summary;
    assert.ok(
      s.inserted === 5 && s.skipped === 0,
      "5 messages archived (got inserted=" + s.inserted + " skipped=" + s.skipped + ")",
    );
    const media = outcome.value.media;
    assert.ok(!!media, "media summary present (extraction ran)");
    assert.ok(
      media.referenced === 4 && media.stored === 4 && media.deduped === 1 && media.missing === 0 && media.linked === 4,
      "media counts 4/4/1/0/4 (got " + JSON.stringify(media) + ")",
    );

    // --- the store on disk: hash-named, real bytes -------------------------
    const photoSha = await sha256Hex(photo);
    const storedPhoto = path.join(storeDir, photoSha + ".jpg");
    assert.ok(existsSync(storedPhoto), "photo stored hash-named under app data media/");
    const storedBytes = await readFile(storedPhoto);
    const sourceBytes = await readFile(photo);
    assert.ok(
      Buffer.compare(storedBytes, sourceBytes) === 0,
      "stored photo bytes identical to the export file",
    );
    const storedNames = await readdir(storeDir);
    assert.equal(storedNames.length, 4, "exactly 4 files in the store");

    // --- detail view renders the media section (real UI surface) ----------
    await browser.execute(() => location.reload());
    await ui.waitForApp(browser);
    // A blank query prompts instead of listing the archive, so the
    // media-bearing messages are reached through author searches.
    let sawVideo = false;
    let opened = false;
    for (const query of ["Marko", "Jelena"]) {
      await ui.search(browser, query);
      const hitCount = (await browser.$$("#results li.hit-item")).length;
      assert.ok(hitCount >= 2, `author search "${query}" lists its messages`);
      for (let i = 0; i < hitCount; i++) {
      // Opening the detail re-renders the result list, so the row is
      // re-resolved and clicked through the DOM every pass (a stale
      // WebdriverIO element gets its click intercepted by its replacement).
      await browser.execute(
        (idx) => document.querySelectorAll("#results li.hit-item")[idx]?.click(),
        i,
      );
      await browser.waitUntil(
        () => browser.execute(() => document.querySelector("#detail.open") !== null),
        { timeoutMsg: "clicking a hit did not open the detail panel" },
      );
      const detailText = await browser.execute(
        () => document.querySelector("#detail")?.textContent ?? "",
      );
      const kinds = await browser.execute(() =>
        Array.from(document.querySelectorAll("#detail .detail-media .media-kind")).map(
          (el) => el.textContent,
        ),
      );
      if (detailText.includes("clip.mp4")) {
        sawVideo = true;
        assert.ok(
          kinds.includes("image") && kinds.includes("video"),
          "video message detail shows image thumbnail and video badges (got " + JSON.stringify(kinds) + ")",
        );
      }
      if (detailText.includes("plan.pdf")) {
        opened = true;
        const badge = kinds[0];
        assert.equal(badge, "document", "document media kind badge");
        const names = await browser.execute(() =>
          Array.from(document.querySelectorAll("#detail .detail-media .media-name")).map(
            (el) => el.textContent,
          ),
        );
        assert.ok(names.includes("plan.pdf"), "media name shown");
        // M4.3: the row shows the humanized size; the store path lives
        // in the mcmedia:// URL (checked by the media-display spec) and
        // the raw JSON, no longer in the row text.
        const metas = await browser.execute(() =>
          Array.from(document.querySelectorAll("#detail .detail-media .media-head .meta")).map(
            (el) => el.textContent,
          ),
        );
        assert.ok(
          metas.some((m) => /^\d+(\.\d+)? (B|KB|MB|GB)$/.test(m)),
          "meta shows the humanized size (got " + JSON.stringify(metas) + ")",
        );
      }
      await browser.execute(() => {
        document.querySelector("#detail")?.classList.remove("open");
      });
      }
    }
    assert.ok(opened, "found the document message in the detail view");
    assert.ok(sawVideo, "video message detail inspected");

    await saveScreenshot(browser, "media-detail.png");

    // --- extraction off: same command, no store writes ---------------------
    const offExport = path.join(dataDir, "tg-off");
    await mkdir(path.join(offExport, "photos"), { recursive: true });
    await copyFile(photo, path.join(offExport, "photos/off.jpg"));
    await writeFile(
      path.join(offExport, "result.json"),
      JSON.stringify({
        name: "off",
        id: 2,
        messages: [
          { id: 1, type: "message", date_unixtime: "1698832800", from: "A",
            photo: "photos/off.jpg", text: "izvuci ovo" },
        ],
      }),
    );
    const offOutcome = await invoke(browser, "import_chat_export", {
      path: offExport,
      extractMedia: false,
    });
    assert.ok(offOutcome.ok, `extract-off import succeeded: ${offOutcome.error ?? ""}`);
    assert.ok(offOutcome.value.media === null, "media summary is null when extraction is off");
    const namesAfterOff = await readdir(storeDir);
    assert.equal(namesAfterOff.length, 4, "store unchanged when extraction is off");
    await ui.search(browser, "izvuci ovo");
    await browser.execute(() =>
      document.querySelector("#results li.hit-item")?.click(),
    );
    const offDetail = await browser.execute(
      () => document.querySelector("#detail")?.innerHTML ?? "",
    );
    assert.ok(
      !offDetail.includes("detail-media"),
      "extract-off message has no media section in the detail view",
    );

    // --- re-import dedup ----------------------------------------------------
    const again = await invoke(browser, "import_chat_export", {
      path: exportDir,
      extractMedia: true,
    });
    assert.ok(again.ok, `re-import succeeded: ${again.error ?? ""}`);
    const rm = again.value.media;
    assert.ok(
      rm.stored === 0 && rm.deduped === 5 && rm.linked === 0,
      "re-import dedups: 0 stored, 5 deduped (4 files + the video thumbnail), 0 relinked (got " +
        JSON.stringify(rm) + ")",
    );

    // --- WhatsApp stem guard ------------------------------------------------
    const waDir = path.join(dataDir, "wa-guard");
    await mkdir(path.join(waDir, "media_docs"), { recursive: true });
    await copyFile(plan, path.join(waDir, "media_docs/plan.pdf.bak"));
    await copyFile(photo, path.join(waDir, "IMG-1.jpeg"));
    const chat = path.join(waDir, "WhatsApp Chat with Ana.txt");
    await writeFile(
      chat,
      "[1.11.2026., 09:00:00] Marko: doc\n" +
        "[1.11.2026., 09:01:00] Ana: plan.pdf (file attached)\n" +
        "[1.11.2026., 09:02:00] Marko: IMG-1.jpg (file attached)\n",
    );
    const wa = await invoke(browser, "import_chat_export", { path: chat, extractMedia: true });
    assert.ok(wa.ok, `whatsapp import succeeded: ${wa.error ?? ""}`);
    const wm = wa.value.media;
    // M4.2 records the missing reference as an explicit missing media
    // entry on its message, so the plan.pdf message's media array is
    // written too — linked counts both messages, not just the stored one.
    assert.ok(
      wm.referenced === 2 && wm.missing === 1 && wm.deduped === 1 && wm.linked === 2,
      "plan.pdf.bak not linked (missing=1); IMG-1.jpg matched IMG-1.jpeg by stem, stored bytes " +
        "dedup onto the existing photo copy (got " + JSON.stringify(wm) + ")",
    );
    await ui.search(browser, "plan.pdf");
    await browser.execute(() =>
      document.querySelector("#results li.hit-item")?.click(),
    );
    const planDetail = await browser.execute(
      () => document.querySelector("#detail")?.textContent ?? "",
    );
    assert.ok(
      planDetail.includes("not in export"),
      "plan.pdf message shows its attachment as unresolved (not in export)",
    );
    assert.ok(
      !planDetail.includes("media/"),
      "plan.pdf message has no stored-media link in the detail view",
    );

    await saveScreenshot(browser, "media-guard.png");
  });
}

// Save a screenshot of the current viewport into the evidence dir when
// one is provided (best-effort: some drivers lack the endpoint).
async function saveScreenshot(browser, filename) {
  const evidenceDir = process.env.MAILCHAT_E2E_EVIDENCE_DIR;
  if (!evidenceDir) {
    return;
  }
  try {
    const base64 = await browser.takeScreenshot();
    const { writeFileSync } = await import("node:fs");
    writeFileSync(path.join(evidenceDir, filename), Buffer.from(base64, "base64"));
    process.stdout.write("screenshot: " + path.join(evidenceDir, filename) + "\n");
  } catch (e) {
    process.stdout.write("screenshot unavailable (" + String(e?.message ?? e) + ")\n");
  }
}
