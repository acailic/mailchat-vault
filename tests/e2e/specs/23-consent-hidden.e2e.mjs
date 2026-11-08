// Path: P0 — the hidden attribute must beat author display rules.
//
// `#results-col .row { display: flex; }` (app.css) overrode the UA
// `[hidden] { display: none }` rule, so #consent-prompt — and its
// "Send & ask for this session" / "Keep asking locally" buttons — were
// physically visible on the very first boot, on an empty archive, before
// any question, contradicting the "nothing leaves this machine" promise
// (cold review §2.3, finding 1). #refresh-notice shares the same
// .row + hidden shape.
//
// This spec asserts DOM geometry (computed display + isDisplayed), not
// just the hidden property: the property was already correct while the
// buttons rendered.

export const name = "consent-hidden-visibility";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // --- fresh boot, empty archive: the consent prompt must be invisible --
    const prompt = await browser.$("#consent-prompt");
    assert.ok(
      (await prompt.getAttribute("hidden")) !== null,
      "consent prompt carries the hidden attribute on first boot",
    );
    assert.ok(
      !(await prompt.isDisplayed()),
      "consent prompt is not displayed on first boot (no question asked)",
    );
    const promptGeometry = await browser.execute(() => {
      const el = document.querySelector("#consent-prompt");
      const rect = el.getBoundingClientRect();
      return {
        display: getComputedStyle(el).display,
        height: rect.height,
        inViewport: rect.top < window.innerHeight && rect.bottom > 0,
      };
    });
    assert.equal(
      promptGeometry.display,
      "none",
      "hidden attribute beats #results-col .row display:flex",
    );
    assert.equal(promptGeometry.height, 0, "hidden consent prompt has no box");
    assert.equal(promptGeometry.inViewport, false, "hidden consent prompt is not in the viewport");

    // The consent buttons must not leak out of the hidden row either.
    assert.ok(
      !(await (await browser.$("#consent-allow-btn")).isDisplayed()),
      "consent allow button invisible on first boot",
    );
    assert.ok(
      !(await (await browser.$("#consent-local-btn")).isDisplayed()),
      "consent local button invisible on first boot",
    );

    // --- an ask on the default (local) provider never opens the prompt ----
    if (await browser.$("#ask").isExisting()) {
      const askInput = await browser.$("#ask");
      await askInput.click();
      await askInput.setValue("anything");
      await browser.keys("Enter");
      await ui.waitForText(
        browser,
        "#ask-status",
        (text) => text !== "" && !text.startsWith("Asking"),
        { timeoutMsg: "local ask never settled" },
      );
      assert.ok(
        !(await prompt.isDisplayed()),
        "consent prompt still hidden after a consent-less local ask",
      );
      const afterAsk = await browser.execute(
        () => getComputedStyle(document.querySelector("#consent-prompt")).display,
      );
      assert.equal(afterAsk, "none", "consent prompt stays display:none after a local ask");
    }

    // --- #refresh-notice: same .row + hidden shape -------------------------
    const notice = await browser.$("#refresh-notice");
    assert.ok(
      !(await notice.isDisplayed()),
      "refresh notice invisible while hidden",
    );
    const noticeDisplay = await browser.execute(
      () => getComputedStyle(document.querySelector("#refresh-notice")).display,
    );
    assert.equal(
      noticeDisplay,
      "none",
      "hidden attribute beats .row display:flex for #refresh-notice",
    );
  });
}
