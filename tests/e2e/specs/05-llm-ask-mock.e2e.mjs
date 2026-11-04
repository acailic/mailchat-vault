// Path: LLM Ask with a mocked /v1/messages endpoint. Proves the full
// pipeline — consent gate → translate → validate → local search →
// summarize — plus the privacy promise (only matched rows are sent), the
// M5 consent contract in the real app (nothing on the wire without the
// session consent; the consented re-ask goes out), and the fail-safe
// fallback when the LLM returns garbage.

export const name = "llm-ask-mock";

export async function run(ctx) {
  const { withSession, assert, ui, startMockLlm } = ctx;

  // Scripted LLM behaviour (only the consented asks ever reach it):
  //  1. translation for the midnight question -> vault + date range that
  //     only the near-midnight pair satisfies
  //  2. summary over the two matched rows
  //  3. translation garbage for the second question -> fallback path
  //  4. summary over the fallback result
  const translateMidnight = JSON.stringify({
    match: '"vault"',
    date_from: "2025-10-31",
    date_to: "2025-11-01",
  });
  const summaryMidnight = "Around Halloween midnight the vault stayed green on both sides [1] [2].";
  const mock = await startMockLlm([
    translateMidnight,
    summaryMidnight,
    "not json at all",
    "The November ledger is ready and reconciled [1].",
  ]);

  try {
    await withSession(
    {
      env: {
        ANTHROPIC_API_KEY: "e2e-key",
        ANTHROPIC_BASE_URL: `http://127.0.0.1:${mock.port}`,
        MAILCHAT_LLM_MODEL: "mock-model",
      },
    },
    async (browser) => {
      await ui.waitForApp(browser);

      if (!(await browser.$("#ask").isExisting())) {
        ctx.skip("Ask UI not present in this build");
      }

      // The privacy note announces LLM mode with the configured model/host,
      // and the M5 consent state line starts a session ungranted.
      const note = await ui.waitForText(browser, "#llm-note", "LLM assist is on");
      assert.includes(note, "mock-model via 127.0.0.1", "note names model and host");
      await ui.waitForText(browser, "#consent-status", "AI consent: not granted");

      await ui.importMbox(browser, ui.archiveMbox);
      await ui.openView(browser, "workspace"); // ask controls are view-scoped

      // --- ask 1: without consent the ask runs fully locally (M5) --------
      const askInput = await browser.$("#ask");
      await askInput.click();
      await askInput.setValue("what happened with the vault around midnight?");
      await browser.keys("Enter");
      const local = await ui.waitForText(
        browser,
        "#ask-status",
        (text) => text !== "" && !text.startsWith("Asking"),
        { timeoutMsg: "consent-less ask never settled" },
      );
      assert.includes(
        local,
        "local full-text — nothing sent to any provider",
        "consent-less ask stays fully local",
      );
      assert.equal(mock.requests.length, 0, "no request without consent (M5 gate)");
      // The per-question consent prompt states what would leave and offers
      // the session grant; agreeing re-asks the same question out.
      const prompt = await browser.$("#consent-prompt");
      assert.ok(await prompt.isDisplayed(), "consent prompt shown");
      const promptText = await (await browser.$("#consent-text")).getText();
      assert.includes(promptText, "mock-model via 127.0.0.1", "prompt names the provider");
      assert.includes(promptText, "top 8 matching messages", "prompt states the scope");
      await (await browser.$("#consent-allow-btn")).click();
      await ui.waitForText(browser, "#consent-status", "granted for this session");

      // --- consented re-ask: LLM-planned vault search, midnight pair ------
      // (Waiting for "planned by LLM" specifically: the consent-less local
      // plan's status lingers until the re-ask starts, and it never
      // contains this marker, so a stale poll cannot satisfy the wait.)
      const plan = await ui.waitForText(browser, "#ask-status", "planned by LLM", {
        timeoutMsg: "consented ask never settled",
      });
      assert.includes(plan, 'FTS: "vault"', "plan shows the FTS expression");
      assert.includes(plan, "dates: 2025-10-31 → 2025-11-01", "plan shows the date range");
      assert.includes(plan, "planned by LLM", "plan came from the LLM");

      // Date range filtered to exactly the near-midnight pair: the 10-30
      // and 11-02 vault mails are excluded by the boundary comparison.
      const rows = await browser.$$("#ask-results li").map((li) => li.getText());
      assert.equal(rows.length, 2, "only the two midnight messages match");
      const joined = rows.join("\n");
      assert.ok(
        joined.includes("before midnight") && joined.includes("after midnight"),
        `both midnight messages shown: ${joined.slice(0, 120)}…`,
      );
      assert.ok(!joined.includes("basalt"), "out-of-range vault mail excluded (11-02)");

      // The RAG answer over the matched rows is displayed, with its
      // citations rendered as clickable source links (M5).
      const answer = await browser.$("#answer");
      assert.ok(await answer.isDisplayed(), "answer box visible");
      assert.equal(await answer.getText(), summaryMidnight, "summary text rendered verbatim");
      const citations = await browser.$$("#answer .citation");
      assert.equal(citations.length, 2, "both citations are clickable");
      // Citation [2] opens the second source message in its conversation
      // context in the reading pane (evidence navigation).
      await citations[1].click();
      await ui.waitForText(browser, "#detail", "midnight");

      // --- ask 2: consent remembered — straight to the LLM; unusable
      // translation falls back to plain term search -----------------------
      await askInput.click();
      await askInput.setValue("ledger november?");
      await browser.keys("Enter");
      // Specific marker again: the consented plan's status lingers until
      // this ask starts, and it never says "unusable".
      const fallback = await ui.waitForText(
        browser,
        "#ask-status",
        "LLM plan unusable",
        { timeoutMsg: "fallback ask never settled" },
      );
      assert.includes(
        fallback,
        "LLM plan unusable — plain term search",
        "fallback plan provenance",
      );
      const fallbackRows = await browser.$$("#ask-results li").map((li) => li.getText());
      assert.equal(fallbackRows.length, 1, "fallback still returns the ledger message");
      assert.ok(fallbackRows[0].includes("ledger"), "fallback result is relevant");

      // The degradation note is visible.
      const notes = await browser.$("#ask-notes");
      assert.ok(await notes.isDisplayed(), "notes visible in fallback mode");
      assert.includes(await notes.getText(), "LLM translation failed", "note explains fallback");

      // The fallback answer was still summarized from the matched row.
      assert.includes(
        await (await browser.$("#answer")).getText(),
        "The November ledger is ready",
        "fallback summary rendered",
      );

      // --- exactly what left the machine ------------------------------------
      // The consent-less ask sent nothing; each consented ask makes two
      // round trips: translate + summarize.
      assert.equal(mock.requests.length, 4, "nothing before consent, then two LLM calls per consented ask");
      for (const [index, request] of mock.requests.entries()) {
        assert.equal(request.body.model, "mock-model", `request ${index + 1} model`);
      }
      assert.equal(
        mock.requests[0].headers["x-api-key"],
        "e2e-key",
        "API key sent as x-api-key header",
      );

      // Privacy scope: the summary prompt contains ONLY the two matched
      // midnight rows — never the whole archive.
      const summaryPrompt = mock.requests[1].body.messages[0].content;
      assert.includes(
        summaryPrompt,
        "Vault status right before midnight",
        "summary prompt includes the matched before-midnight row",
      );
      assert.ok(
        !summaryPrompt.includes("basalt"),
        "summary prompt excludes out-of-range vault mail (11-02)",
      );
      assert.ok(
        !summaryPrompt.includes("Granite milestone"),
        "summary prompt excludes out-of-range vault mail (10-30)",
      );
      assert.ok(
        !summaryPrompt.includes("zephyr"),
        "summary prompt excludes non-matching messages entirely",
      );
    },
  );
  } finally {
    await mock.close();
  }
}
