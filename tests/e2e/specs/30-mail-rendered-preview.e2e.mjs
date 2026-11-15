// Path: packaged-build rendered-email preview smoke (#89 follow-up, #102
// leg C). An .mbox whose mail carries a text/html alternative is
// imported; the reading pane's Rendered mode must show the sender HTML
// inside the sandboxed srcdoc iframe with the app's packaged CSP in
// force — the inline data: image actually decodes and renders, and the
// link inside the frame is inert (clicking it navigates nothing: the
// frame stays on about:srcdoc and the app window is unchanged). The
// vitest suite (src/render2.test.ts) can only pin the srcdoc *attribute
// strings*; whether WebKitGTK really renders the image under the
// packaged build's CSP difference is exactly what this spec proves.
//
// Skip contract (reports PENDING, never a false failure, on machines
// that cannot run it — see docs/testing.md tiers): it skips before
// opening a session when the app binary or the WebKitWebDriver binary
// is absent (tier B prerequisites), and it skips inside the session
// when the running build serves its assets from a dev server
// (http://localhost) instead of the packaged/embedded tauri: origin,
// because the CSP surface under test only exists in the embedded-assets
// build (`cargo build -p mailchat-vault --features
// tauri/custom-protocol` or a released bundle via MAILCHAT_E2E_BINARY).
export const name = "mail-rendered-preview";

import { existsSync } from "node:fs";
import path from "node:path";
import { appBinary } from "../harness/app.mjs";
import { fixturesDir } from "../harness/ui.mjs";

/** The native WebKitWebDriver tauri-driver proxies to (same candidate
 * order as the harness's own lookup — kept local because the harness
 * does not export it; a MAILCHAT_WEBKIT_DRIVER override always wins). */
function webkitDriverPresent() {
  const override = process.env.MAILCHAT_WEBKIT_DRIVER;
  if (override) {
    return existsSync(override);
  }
  return (
    existsSync("/usr/libexec/webkit2gtk-4.1/WebKitWebDriver") ||
    existsSync("/usr/bin/WebKitWebDriver")
  );
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;
  const binary = appBinary();
  if (!existsSync(binary)) {
    ctx.skip(
      `pending (tier B): app binary not built (${binary}) — build with ` +
        "cargo build -p mailchat-vault --features tauri/custom-protocol " +
        "or point MAILCHAT_E2E_BINARY at a packaged binary",
    );
  }
  if (!webkitDriverPresent()) {
    ctx.skip(
      "pending (tier B): no WebKitWebDriver binary — install the " +
        "webkit2gtk-driver (or webkitgtk-webdriver) package on a Linux " +
        "webkit2gtk machine",
    );
  }
  const mbox = path.join(fixturesDir, "rendered-email.mbox");

  await withSession({}, async (browser) => {
    const origin = await browser.execute(() => location.origin);
    if (!origin.startsWith("tauri:")) {
      ctx.skip(
        `pending (tier B/C): this build serves assets from ${origin} ` +
        "(a dev server), not the packaged/embedded tauri: origin — the " +
        "packaged-CSP surface under test is absent; rebuild with the " +
        "tauri/custom-protocol feature or drive a packaged binary",
      );
    }

    await ui.waitForApp(browser);
    const status = await ui.importMbox(browser, mbox);
    assert.deepEqual(
      status,
      { source: "mbox", seen: 1, inserted: 1, skipped: 0, failed: 0 },
      "import summary",
    );

    // Open the fixture mail in the reading pane.
    await ui.search(browser, "beacon");
    await browser.execute(() =>
      document.querySelector("#results li.hit-item")?.click(),
    );
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail.open") !== null),
      { timeoutMsg: "clicking the hit did not open the reading pane" },
    );

    // Rendered mode is the default for HTML mail: the sandboxed frame
    // exists, the CSP and link-retargeting prefixes are in the srcdoc.
    const buttons = await browser.execute(() =>
      Array.from(document.querySelectorAll("button.mode-btn")).map((b) => b.textContent),
    );
    assert.deepEqual(buttons, ["Rendered", "Raw"], "Rendered/Raw toggle present");
    const srcdoc = await browser.execute(() =>
      document.querySelector("#detail iframe.rendered-frame")?.getAttribute("srcdoc") ?? "",
    );
    assert.ok(srcdoc.includes("default-src 'none'"), "srcdoc carries the blocking CSP");
    assert.ok(srcdoc.includes("img-src data:"), "srcdoc CSP allows data: images only");
    assert.ok(
      srcdoc.includes('<base target="_blank">'),
      "links are retargeted to windows the sandbox never opens",
    );

    // --- inside the frame: the packaged-CSP leg vitest cannot see.
    const frame = await browser.$("#detail iframe.rendered-frame");
    await browser.switchToFrame(frame);
    const imageState = await browser.execute(() => {
      const img = document.querySelector("img[alt='quarterly beacon chart']");
      if (!img) {
        return { present: false };
      }
      const rect = img.getBoundingClientRect();
      return {
        present: true,
        complete: img.complete,
        naturalWidth: img.naturalWidth,
        naturalHeight: img.naturalHeight,
        renderedWidth: rect.width,
        renderedHeight: rect.height,
      };
    });
    assert.ok(imageState.present, "the data: image exists inside the frame");
    assert.ok(
      imageState.complete && imageState.naturalWidth === 8 && imageState.naturalHeight === 8,
      "the data: image actually decoded under the packaged CSP " +
        `(complete=${imageState.complete}, natural=${imageState.naturalWidth}x${imageState.naturalHeight})`,
    );
    assert.ok(
      imageState.renderedWidth > 0 && imageState.renderedHeight > 0,
      "the decoded image occupies layout (is visible)",
    );

    // The link click must stay inert: <base target="_blank"> meets the
    // popup-less sandbox, so neither the frame nor the app navigates.
    const beforeHref = await browser.execute(() => location.href);
    await browser.execute(() =>
      document.querySelector("a[href='https://example.com/never-followed-beacon']")?.click(),
    );
    let navigated = false;
    try {
      await browser.waitUntil(
        async () => (await browser.execute(() => location.href)) !== beforeHref,
        { timeout: 2_000 },
      );
      navigated = true;
    } catch {
      // still on the same document — the expected inert outcome
    }
    assert.ok(!navigated, "clicking the frame's link navigated the frame (must be inert)");
    assert.equal(
      await browser.execute(() => document.querySelector("img")?.complete ?? false),
      true,
      "frame content unchanged after the link click",
    );
    await browser.switchToFrame(null);

    // The app window itself never moved either.
    const originAfter = await browser.execute(() => location.origin);
    assert.equal(originAfter, origin, "the app window did not navigate");
    const detailStillOpen = await browser.execute(() =>
      document.querySelector("#detail.open") !== null,
    );
    assert.ok(detailStillOpen, "the reading pane is still open after the link click");

    // Raw mode drops the frame; Rendered brings it back (parity with
    // the unit tests, here against the real webview).
    await browser.execute(() =>
      Array.from(document.querySelectorAll("button.mode-btn"))
        .find((b) => b.textContent === "Raw")
        ?.click(),
    );
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail iframe.rendered-frame") === null),
      { timeoutMsg: "Raw mode did not drop the sandboxed frame" },
    );
    await browser.execute(() =>
      Array.from(document.querySelectorAll("button.mode-btn"))
        .find((b) => b.textContent === "Rendered")
        ?.click(),
    );
    await browser.waitUntil(
      () => browser.execute(() => document.querySelector("#detail iframe.rendered-frame") !== null),
      { timeoutMsg: "Rendered mode did not restore the sandboxed frame" },
    );
  });
}
