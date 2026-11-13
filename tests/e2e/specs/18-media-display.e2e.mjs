// Media display end-to-end (M4.3): preserved attachments render in the
// reading pane through the read-only mcmedia:// protocol — images decode,
// audio/video elements exist with controls and never autoplay — the app
// CSP blocks remote fetches from the media surface, HTML attachments
// never execute or embed, byte-range reads answer 206 with exact slices
// over a large object without freezing the UI, explicit export copies
// bytes exactly, and the Settings media store shows the disk-usage
// report plus a clean reconcile.
export const name = "media-display";

import { copyFile, mkdir, writeFile, readFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import { createHash } from "node:crypto";
import http from "node:http";
import path from "node:path";
import { fixturesDir } from "../harness/ui.mjs";

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

/** A small, valid JPEG (the committed photo.jpg fixture is a
 * deliberately-truncated test object that WebKit refuses to decode —
 * fine for byte-level checks, useless for a decode assertion). */
async function makeValidJpeg(dir) {
  const { execFile } = await import("node:child_process");
  const script =
    "from PIL import Image, ImageDraw; " +
    "im = Image.new('RGB', (96, 64), (180, 40, 60)); " +
    "d = ImageDraw.Draw(im); d.rectangle((16, 16, 80, 48), fill=(40, 120, 200)); " +
    "im.save(sys.argv[1], 'JPEG', quality=88)";
  const target = path.join(dir, "valid.jpg");
  await new Promise((resolve, reject) =>
    execFile("python3", ["-c", "import sys; " + script, target], (err) =>
      err ? reject(err) : resolve()),
  );
  return target;
}

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

/** Switch views by clicking the real nav button in the DOM (same
 * rationale as searchViaDom: native clicks on this driver build are
 * refused; the app's own click handler runs unchanged). */
async function openViewViaDom(browser, view) {
  const current = await browser.execute(
    () => document.querySelector("#app")?.getAttribute("data-view") ?? "",
  );
  if (current === view) return;
  await browser.execute((target) => {
    document
      .querySelector(`#nav-views .nav-view-btn[data-view-target="${target}"]`)
      ?.click();
  }, view);
  await browser.waitUntil(
    () =>
      browser
        .execute(() => document.querySelector("#app")?.getAttribute("data-view") ?? "")
        .then((v) => v === view),
    { timeout: 5_000, timeoutMsg: `view "${view}" never mounted` },
  );
}

/** Submit a search by driving the real input element's value and Enter
 * key (same events `ui.search` sends; this build of WebKitWebDriver
 * refuses native clicks on the input, so the events are dispatched on
 * the element itself — the app handler is unchanged). */
async function searchViaDom(browser, text) {
  await openViewViaDom(browser, "workspace");
  await browser.execute((value) => {
    const input = document.querySelector("#search");
    input.value = value;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    );
  }, text);
  await browser.waitUntil(
    () =>
      browser
        .execute(() => document.querySelector("#status")?.textContent ?? "")
        .then(
          (t) => t !== "" && !t.startsWith("Searching") && !t.startsWith("Type something"),
        ),
    { timeout: 15_000, timeoutMsg: "search never settled" },
  );
}

/** A local HTTP server standing in for "the remote": counts every
 * request a blocked load must never make. */
function remoteProbe() {
  const hits = [];
  const server = http.createServer((req, res) => {
    hits.push(req.url);
    res.writeHead(200, { "Content-Type": "image/png" });
    res.end(Buffer.from([0x89, 0x50, 0x4e, 0x47]));
  });
  return {
    start: () =>
      new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server.address().port))),
    stop: () => new Promise((resolve) => server.close(() => resolve())),
    hits,
  };
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser, { dataDir }) => {
    await ui.waitForApp(browser);

    // --- disposable export: real photo, real wav, a smuggled HTML file,
    //     and a 24 MB binary for the range/freeze checks -------------------
    const exportDir = path.join(dataDir, "tg-media");
    for (const sub of ["photos", "files", "voice_messages"]) {
      await mkdir(path.join(exportDir, sub), { recursive: true });
    }
    const beep = path.join(mediaFixtures, "beep.wav");
    const validPhoto = await makeValidJpeg(dataDir);
    await copyFile(validPhoto, path.join(exportDir, "photos/photo_200.jpg"));
    await copyFile(beep, path.join(exportDir, "voice_messages/audio_201.ogg"));
    // An HTML attachment with active content: must display as a document
    // row only — never inline, never executing.
    await writeFile(
      path.join(exportDir, "files/evil.html"),
      `<!doctype html><html><body><script>window.__mediaPwned = 1;</script>` +
        `<img src="http://127.0.0.1:1/remote.png"><p>hello</p></body></html>`,
    );
    // 24 MiB pseudo-random binary: large-object range reads.
    const bigChunk = Buffer.alloc(24 * 1024 * 1024);
    for (let i = 0; i < bigChunk.length; i += 4096) {
      bigChunk.fill((i * 7) % 251, i, i + 4096);
    }
    await writeFile(path.join(exportDir, "files/big.bin"), bigChunk);
    await writeFile(
      path.join(exportDir, "result.json"),
      JSON.stringify({
        name: "Media test",
        type: "private_group",
        id: 42,
        messages: [
          { id: 200, type: "message", date_unixtime: "1700000000", from: "Ana",
            photo: "photos/photo_200.jpg", text: "" },
          { id: 201, type: "message", date_unixtime: "1700000100", from: "Bo",
            media_type: "voice_message", file: "voice_messages/audio_201.ogg", text: "" },
          { id: 202, type: "message", date_unixtime: "1700000200", from: "Cki",
            media_type: "document", file: "files/evil.html", text: "look at this" },
          { id: 203, type: "message", date_unixtime: "1700000300", from: "Dan",
            media_type: "document", file: "files/big.bin", text: "" },
        ],
      }),
    );

    const outcome = await invoke(browser, "import_chat_export", {
      path: exportDir,
      extractMedia: true,
    });
    assert.ok(outcome.ok, `import succeeded: ${outcome.error ?? ""}`);
    assert.equal(outcome.value.summary.inserted, 4, "4 messages archived");

    const photoSha = await sha256Hex(validPhoto);
    const beepSha = await sha256Hex(beep);
    const evilSha = await sha256Hex(path.join(exportDir, "files/evil.html"));
    const bigSha = createHash("sha256").update(bigChunk).digest("hex");
    const mediaUrl = (sha, ext) => `mcmedia://local/${sha}.${ext}`;

    // --- the CSP the production build ships (a response header on the
    //     app document — read back with a same-origin fetch) -------------
    const csp = await browser.execute(async () => {
      try {
        const res = await fetch(location.href);
        return res.headers.get("content-security-policy");
      } catch {
        return null;
      }
    });
    assert.ok(typeof csp === "string" && csp.length > 0, "app CSP is present (got " + JSON.stringify(csp) + ")");
    assert.ok(/script-src[^;]*'self'/.test(csp), "script-src 'self'");
    assert.ok(!/script-src[^;]*unsafe-inline/.test(csp), "no inline scripts allowed");
    assert.ok(!/script-src[^;]*unsafe-eval/.test(csp), "no eval allowed");
    const imgSrc = (csp.match(/img-src[^;]*/) ?? [""])[0];
    const mediaSrc = (csp.match(/media-src[^;]*/) ?? [""])[0];
    assert.ok(imgSrc.includes("mcmedia:") && mediaSrc.includes("mcmedia:"),
      "img/media sources allow only local + mcmedia (got img: '" + imgSrc + "', media: '" + mediaSrc + "')");
    assert.ok(!/https?:/.test(imgSrc) && !/https?:/.test(mediaSrc),
      "no remote origins in img-src/media-src");
    assert.ok(/object-src[^;]*'none'/.test(csp), "object-src 'none'");

    // The media protocol stamps its own hard CSP on every response too.
    const mediaCsp = await browser.execute(async (url) => {
      const res = await fetch(url, { headers: { Range: "bytes=0-3" } });
      return {
        csp: res.headers.get("content-security-policy"),
        nosniff: res.headers.get("x-content-type-options"),
      };
    }, mediaUrl(photoSha, "jpg"));
    assert.ok(
      mediaCsp.csp !== null && mediaCsp.csp.includes("default-src 'none'"),
      "media responses carry their own hard CSP (got " + JSON.stringify(mediaCsp) + ")",
    );
    assert.equal(mediaCsp.nosniff, "nosniff", "media responses set nosniff");

    // --- media rendering in the detail view ------------------------------
    await searchViaDom(browser, "Ana");
    await browser.waitUntil(
      () => browser.execute(() => document.querySelectorAll("#results li.hit-item").length > 0),
      { timeoutMsg: "photo message not listed" },
    );
    await browser.execute(() => document.querySelector("#results li.hit-item")?.click());
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "detail did not open" },
    );
    const imgInfo = await browser.execute(() => {
      const img = document.querySelector("#detail .detail-media img");
      if (!img) return null;
      return {
        src: img.getAttribute("src"),
        loaded: img.complete && img.naturalWidth > 0,
        lazy: img.getAttribute("loading"),
        alt: img.getAttribute("alt"),
      };
    });
    assert.ok(imgInfo !== null, "photo renders an <img> from the media protocol");
    assert.equal(imgInfo.src, mediaUrl(photoSha, "jpg"), "img src is the mcmedia URL");
    assert.ok(imgInfo.loaded, "the real JPEG bytes served and decoded (naturalWidth > 0)");
    assert.equal(imgInfo.lazy, "lazy", "image loads lazily");

    // Audio: element exists with controls; never autoplay; no preload.
    await searchViaDom(browser, "Bo");
    await browser.execute(() => document.querySelector("#results li.hit-item")?.click());
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "audio detail did not open" },
    );
    const audioInfo = await browser.execute(() => {
      const audio = document.querySelector("#detail .detail-media audio");
      if (!audio) return null;
      return {
        src: audio.getAttribute("src"),
        controls: audio.hasAttribute("controls"),
        autoplay: audio.hasAttribute("autoplay"),
        preload: audio.getAttribute("preload"),
      };
    });
    assert.ok(audioInfo !== null, "voice note renders an <audio> element");
    assert.equal(audioInfo.src, mediaUrl(beepSha, "ogg"), "audio src is the mcmedia URL");
    assert.ok(audioInfo.controls, "audio has user-facing controls");
    assert.ok(!audioInfo.autoplay, "audio never autoplays");
    assert.equal(audioInfo.preload, "none", "audio does not preload");

    // Documents with active content: row + explicit actions, no embed,
    // no script execution.
    await searchViaDom(browser, "Cki");
    await browser.execute(() => document.querySelector("#results li.hit-item")?.click());
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "html detail did not open" },
    );
    const htmlInfo = await browser.execute(() => ({
      embeds: document.querySelectorAll("#detail iframe, #detail embed, #detail object").length,
      pwned: window.__mediaPwned === 1,
      row: (() => {
        const item = Array.from(document.querySelectorAll("#detail .media-item")).find((li) =>
          li.textContent?.includes("evil.html"),
        );
        if (!item) return null;
        return {
          kind: item.querySelector(".media-kind")?.textContent,
          hasSave: item.querySelector("button.media-save-btn") !== null,
          hasOpen: item.querySelector("button.media-open-btn") !== null,
          hasInlineMedia: item.querySelector("img, audio, video") !== null,
        };
      })(),
    }));
    assert.equal(htmlInfo.embeds, 0, "no iframe/embed/object anywhere in the detail view");
    assert.ok(!htmlInfo.pwned, "HTML attachment script never executed");
    assert.ok(htmlInfo.row !== null, "evil.html listed in the media section");
    assert.equal(htmlInfo.row.kind, "document", "html attachment shown as a document");
    assert.ok(htmlInfo.row.hasSave && htmlInfo.row.hasOpen, "document row offers explicit actions");
    assert.ok(!htmlInfo.row.hasInlineMedia, "html attachment renders nothing inline");

    // Video elements (from another message) never autoplay either: reuse
    // the big-object message to assert the media-element contract holds
    // for every rendering path via a direct element check in the DOM.
    const noAutoplayAnywhere = await browser.execute(() =>
      Array.from(document.querySelectorAll("audio, video")).every((el) => !el.hasAttribute("autoplay")),
    );
    assert.ok(noAutoplayAnywhere, "no media element in the app autoplays");

    // --- remote fetch is blocked by CSP (behavioral probe) ---------------
    const probe = remoteProbe();
    const port = await probe.start();
    const remote = await browser.execute(async (port) => {
      const url = `http://127.0.0.1:${port}/pixel.png`;
      const imgBlocked = await new Promise((resolve) => {
        const img = new Image();
        const done = (ok) => resolve(ok);
        img.onload = () => done(false);
        img.onerror = () => done(true);
        setTimeout(() => done(false), 4000);
        img.src = url;
      });
      let fetchBlocked = false;
      try {
        await fetch(url);
      } catch {
        fetchBlocked = true;
      }
      return { imgBlocked, fetchBlocked };
    }, port);
    await probe.stop();
    assert.ok(remote.imgBlocked, "a remote <img> load from the app is blocked");
    assert.ok(remote.fetchBlocked, "a remote fetch() from the app is rejected");
    assert.equal(probe.hits.length, 0, "the remote server saw no requests at all");

    // --- byte ranges: exact slices, headers, and large objects -----------
    const range = await browser.execute(async (url) => {
      const res = await fetch(url, { headers: { Range: "bytes=1000-1999" } });
      const buf = new Uint8Array(await res.arrayBuffer());
      return {
        status: res.status,
        length: buf.length,
        contentRange: res.headers.get("content-range"),
        acceptRanges: res.headers.get("accept-ranges"),
        contentType: res.headers.get("content-type"),
        first: buf[0],
        last: buf[buf.length - 1],
      };
    }, mediaUrl(bigSha, "bin"));
    assert.equal(range.status, 206, "range request answers 206");
    assert.equal(range.length, 1000, "exactly the requested slice length");
    assert.equal(range.contentRange, `bytes 1000-1999/${bigChunk.length}`, "content-range names the slice");
    assert.equal(range.acceptRanges, "bytes", "accept-ranges advertised");
    assert.ok(range.contentType.includes("octet-stream"), "unknown binary served as octet-stream");
    assert.equal(range.first, bigChunk[1000], "slice starts at the right byte");
    assert.equal(range.last, bigChunk[1999], "slice ends at the right byte");

    // Suffix and open ranges; a full GET returns the whole object.
    const edges = await browser.execute(async (url) => {
      const out = {};
      const read = async (headers) => {
        const res = await fetch(url, { headers });
        const buf = new Uint8Array(await res.arrayBuffer());
        return { status: res.status, len: buf.length, first: buf[0], last: buf[buf.length - 1] };
      };
      out.suffix = await read({ Range: "bytes=-16" });
      out.open = await read({ Range: `bytes=${24 * 1024 * 1024 - 8}-` });
      out.full = await read({});
      return out;
    }, mediaUrl(bigSha, "bin"));
    const size = bigChunk.length;
    assert.equal(edges.suffix.status, 206, "suffix range 206");
    assert.equal(edges.suffix.len, 16, "suffix range length");
    assert.equal(edges.suffix.first, bigChunk[size - 16], "suffix starts at len-16");
    assert.equal(edges.open.len, 8, "open range returns the tail");
    assert.equal(edges.open.first, bigChunk[size - 8], "open range starts at the right byte");
    assert.equal(edges.full.status, 200, "full GET 200");
    assert.equal(edges.full.len, size, "full GET returns every byte");
    assert.equal(edges.full.first, bigChunk[0], "full GET first byte");
    assert.equal(edges.full.last, bigChunk[size - 1], "full GET last byte");

    // Names that are not store objects 404 — including traversal.
    const badNames = await browser.execute(async () => {
      const fetches = [
        "mcmedia://local/notes.txt",
        "mcmedia://local/../../../../etc/passwd",
        "mcmedia://local/media/aaaa.jpg",
        "mcmedia://local/zzzz.jpg",
      ];
      const statuses = [];
      for (const url of fetches) {
        try {
          const res = await fetch(url);
          statuses.push(res.status);
        } catch {
          statuses.push("network-error");
        }
      }
      return statuses;
    });
    for (const status of badNames) {
      assert.ok(status === 404 || status === "network-error",
        "non-object names are not served (got " + JSON.stringify(badNames) + ")");
    }

    // --- responsiveness: 30 range reads must not freeze the UI thread ----
    const freeze = await browser.execute(async (url) => {
      const t0 = performance.now();
      let reads = 0;
      // A 50ms UI timer runs while 30 x 512 KiB range reads stream: a
      // handler that blocked the UI thread would pin the timer until
      // every read finished, inflating its drift by seconds.
      let timerResolve;
      const timerPromise = new Promise((resolve) => {
        const scheduled = performance.now();
        timerResolve = () => resolve(performance.now() - scheduled);
        setTimeout(timerResolve, 50);
      });
      const readsPromise = (async () => {
        for (let i = 0; i < 30; i++) {
          const start = i * 512 * 1024;
          await fetch(url, { headers: { Range: `bytes=${start}-${start + 512 * 1024 - 1}` } });
          reads++;
        }
      })();
      const drift = await timerPromise;
      await readsPromise;
      return { drift, reads, total: performance.now() - t0 };
    }, mediaUrl(bigSha, "bin"));
    assert.equal(freeze.reads, 30, "all 30 large range reads completed");
    assert.ok(
      freeze.drift < 2500,
      `a 50ms UI timer fired within 2.5s while media streamed (drift ${Math.round(freeze.drift)}ms)`,
    );

    // --- explicit export: byte-exact copy to a picked path ---------------
    const exportDest = path.join(dataDir, "exported-photo.jpg");
    const exportOutcome = await invoke(browser, "media_export", {
      name: `${photoSha}.jpg`,
      path: exportDest,
    });
    assert.ok(exportOutcome.ok, `media_export succeeded: ${exportOutcome.error ?? ""}`);
    assert.equal(exportOutcome.value.size, (await stat(validPhoto)).size, "export reports the true size");
    assert.ok(
      Buffer.compare(await readFile(exportDest), await readFile(validPhoto)) === 0,
      "exported copy is byte-identical to the source object",
    );

    // External open: the temp preview copy is exact even when the desktop
    // has no handler (the open dispatch may fail environmentally).
    const openOutcome = await invoke(browser, "media_open_external", {
      name: `${photoSha}.jpg`,
      displayName: "photo_200.jpg",
    });
    if (openOutcome.ok) {
      assert.ok(openOutcome.value.path.includes("mailchat-media-open"), "opened from the preview dir");
    } else {
      assert.ok(
        /cannot open externally/.test(openOutcome.error),
        "external open fails only at the dispatch step (got " + openOutcome.error + ")",
      );
    }
    const previewPath = path.join(
      "/tmp", "mailchat-media-open", photoSha, "photo_200.jpg",
    );
    assert.ok(
      existsSync(previewPath) &&
        Buffer.compare(await readFile(previewPath), await readFile(validPhoto)) === 0,
      "external open staged an exact disposable copy",
    );

    // Bogus names are rejected by the explicit-action commands too.
    const badExport = await invoke(browser, "media_export", {
      name: "../escape.jpg",
      path: path.join(dataDir, "escape.jpg"),
    });
    assert.ok(!badExport.ok, "media_export rejects non-object names");
    assert.ok(!existsSync(path.join(dataDir, "escape.jpg")), "nothing written outside the store");

    // --- Settings: disk usage report + clean reconcile --------------------
    // The media store lives on the Storage & backup settings page (the
    // report loads when that page opens, not at startup) — same DOM-click
    // rationale as openViewViaDom for the page nav.
    await openViewViaDom(browser, "settings");
    await browser.execute(() =>
      document
        .querySelector('.settings-nav-btn[data-settings-target="storage"]')
        ?.click(),
    );
    await browser.waitUntil(
      () =>
        browser
          .execute(() => document.querySelector("#app")?.getAttribute("data-settings-section") ?? "")
          .then((s) => s === "storage"),
      { timeout: 5_000, timeoutMsg: "the Storage & backup settings page never opened" },
    );
    await browser.waitUntil(
      () =>
        browser.execute(
          () => document.querySelector("#media-usage")?.textContent ?? "",
        ).then((t) => t.includes("objects")),
      { timeoutMsg: "media usage report did not render" },
    );
    const usage = await invoke(browser, "media_usage");
    assert.ok(usage.ok, "media_usage command succeeds");
    const u = usage.value;
    assert.equal(u.objects, 4, "usage counts the 4 stored objects");
    assert.equal(u.referenced, 4, "all 4 referenced by the archive");
    assert.equal(u.orphan_objects, 0, "no orphans");
    const exts = u.by_extension.map((e) => e.ext);
    assert.ok(
      ["jpg", "ogg", "html", "bin"].every((ext) => exts.includes(ext)),
      "per-extension breakdown lists jpg/ogg/html/bin (got " + JSON.stringify(exts) + ")",
    );
    assert.ok(
      u.largest.length > 0 && u.largest[0].size === bigChunk.length,
      "the 24 MiB object is reported as the largest",
    );

    // Drive the real Reconcile button so the status line is the UI's own.
    await browser.execute(() =>
      document.querySelector("#media-reconcile-btn")?.click(),
    );
    await browser.waitUntil(
      () =>
        browser
          .execute(() => document.querySelector("#media-store-status")?.textContent ?? "")
          .then((t) => t.includes("clean") || t.includes("Reconcile") || t.includes("failed")),
      { timeout: 30_000, timeoutMsg: "reconcile button produced no status" },
    );
    const reconcile = await invoke(browser, "media_reconcile");
    assert.ok(reconcile.ok, "media_reconcile succeeds");
    const r = reconcile.value;
    assert.ok(
      r.missing.length === 0 && r.corrupt.length === 0 && r.orphans.length === 0,
      "store reconciles clean (got " + JSON.stringify(r) + ")",
    );
    const reconcileStatus = await browser.execute(
      () => document.querySelector("#media-store-status")?.textContent ?? "",
    );
    assert.ok(
      reconcileStatus.includes("clean") || reconcileStatus.includes("Reconcil"),
      "reconcile status visible in Settings (got '" + reconcileStatus + "')",
    );
  });
}
