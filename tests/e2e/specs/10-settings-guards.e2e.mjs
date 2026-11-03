// Path: Settings adversarial guards, end to end. Complements
// 09-settings-providers with the guard scenarios the review asked to see
// driven live: the form configures a provider (settings.json holds the
// profile, never the key), the provider menu appears in Ask, per-ask local
// mode sends nothing, a mis-paired provider is refused BEFORE saving
// (file untouched), a plaintext http non-loopback URL is refused before
// saving, clearing the key disables the menu entry, and a
// parseable-but-invalid settings.json is refused at LOAD with a visible
// error while the app stays functional (second app session over the same
// data dir).

import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

/** Set a <select> and fire change (WebKit session lacks select helpers). */
async function setSelect(browser, selector, value) {
  await browser.execute(
    (sel, val) => {
      const el = document.querySelector(sel);
      el.value = val;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    selector,
    value,
  );
  await browser.waitUntil(async () => (await browser.$(selector).getValue()) === value, {
    timeout: 10_000,
    timeoutMsg: `select ${selector} never took value ${value}`,
  });
}

export const name = "settings-guards";

export async function run(ctx) {
  const { withSession, assert, ui, startMockLlm } = ctx;

  const mock = await startMockLlm([
    JSON.stringify({ match: '"vault"', date_from: null, date_to: null }),
  ]);

  try {
    let firstDataDir = null;

    // ------------------------------------------------------------------
    // Session 1: the happy path + every save-time guard.
    // ------------------------------------------------------------------
    await withSession(
      { env: { ZAI_API_KEY: "", GLM_API_KEY: "", ANTHROPIC_API_KEY: "", ANTHROPIC_BASE_URL: "" } },
      async (browser, { dataDir }) => {
        firstDataDir = dataDir;
        await ui.waitForApp(browser);

        // --- the form configures a provider --------------------------------
        await setSelect(browser, "#add-kind", "custom");
        await browser.$("#add-provider-btn").click();
        const row = await browser.$('.provider-row[data-id="custom"]');
        await row.$(".prov-name").setValue("Mock Lab");
        await row.$(".prov-url").setValue(`http://127.0.0.1:${mock.port}/api`);
        await row.$(".prov-model").setValue("settings-model");
        await row.$(".prov-key").setValue("guard-key");
        await row.$('input[type="radio"]').click();
        await browser.$("#settings-save-btn").click();
        await ui.waitForText(browser, "#settings-note", "Saved.");

        // settings.json holds the profile but never the key.
        const settingsJson = JSON.parse(
          await readFile(
            path.join(dataDir, "com.acailic.mailchatvault", "settings.json"),
            "utf8",
          ),
        );
        assert.equal(settingsJson.providers.length, 1, "one provider persisted");
        assert.equal(settingsJson.providers[0].id, "custom");
        assert.equal(settingsJson.default_provider, "custom");
        const rawFile = JSON.stringify(settingsJson);
        assert.ok(!rawFile.includes("guard-key"), "settings file never contains the key");

        // --- the provider menu appears in Ask ------------------------------
        const askProvider = await browser.$("#ask-provider");
        const options = await askProvider.$$("option");
        assert.equal(options.length, 2, "local + the configured provider");
        assert.equal(await askProvider.getValue(), "custom", "default preselected");
        await ui.waitForText(browser, "#llm-note", "LLM assist is on");

        // --- an ask through the provider makes exactly one LLM call --------
        const ask = await browser.$("#ask");
        await ask.click();
        await ask.setValue("anything about the vault?");
        await browser.keys("Enter");
        const plan = await ui.waitForText(
          browser,
          "#ask-status",
          (t) => t !== "" && !t.startsWith("Asking"),
          { timeoutMsg: "provider ask never settled" },
        );
        assert.includes(plan, "planned by LLM", "provider ask used the LLM");
        assert.equal(mock.requests.length, 1, "one translation call (no results → no summary)");
        assert.equal(
          mock.requests[0].headers["x-api-key"],
          "guard-key",
          "key sent only to the configured endpoint",
        );

        // --- per-ask local mode sends nothing ------------------------------
        await setSelect(browser, "#ask-provider", "local");
        await ui.waitForText(browser, "#llm-note", "LLM assist is off");
        await ask.click();
        await ask.setValue("vault, but locally");
        await browser.keys("Enter");
        const localPlan = await ui.waitForText(
          browser,
          "#ask-status",
          (t) => t !== "" && !t.startsWith("Asking"),
          { timeoutMsg: "local ask never settled" },
        );
        assert.includes(localPlan, "local full-text", "local ask is explicit local mode");
        assert.equal(mock.requests.length, 1, "local mode sends nothing");

        // --- mis-paired provider refused BEFORE saving ----------------------
        await setSelect(browser, "#add-kind", "zai");
        await browser.$("#add-provider-btn").click();
        const zaiRow = await browser.$('.provider-row[data-id="zai"]');
        await zaiRow.$(".prov-url").setValue("https://api.anthropic.com");
        await browser.$("#settings-save-btn").click();
        const misPair = await ui.waitForText(browser, "#settings-note", "Not saved");
        assert.includes(
          misPair,
          "only ever sent to the z.ai endpoint",
          "mis-pairing refused with the pairing error",
        );
        const afterMisPair = JSON.parse(
          await readFile(
            path.join(dataDir, "com.acailic.mailchatvault", "settings.json"),
            "utf8",
          ),
        );
        assert.equal(
          afterMisPair.providers.length,
          1,
          "refused save leaves settings.json untouched",
        );
        await zaiRow.$(".remove-prov-btn").click(); // clean the form

        // --- plaintext http non-loopback refused BEFORE saving -------------
        // (re-query: the successful save earlier replaced the row element)
        await (
          await browser.$('.provider-row[data-id="custom"] .prov-url')
        ).setValue("http://attacker.example/api");
        await browser.$("#settings-save-btn").click();
        const plaintext = await ui.waitForText(browser, "#settings-note", "Not saved");
        assert.includes(
          plaintext,
          "plaintext http",
          "non-loopback plaintext http refused at save",
        );
        const afterPlaintext = JSON.parse(
          await readFile(
            path.join(dataDir, "com.acailic.mailchatvault", "settings.json"),
            "utf8",
          ),
        );
        assert.equal(
          afterPlaintext.providers[0].base_url,
          `http://127.0.0.1:${mock.port}/api`,
          "settings.json keeps the last valid URL",
        );

        // --- clearing the key disables the menu entry ----------------------
        await browser.$('.provider-row[data-id="custom"] .clear-key-btn').click();
        const customOption = await askProvider.$('option[value="custom"]');
        await browser.waitUntil(
          async () => (await customOption.getText()).includes("no API key"),
          { timeout: 15_000, timeoutMsg: "menu entry never reflected the cleared key" },
        );
        const entryDisabled = await browser.execute(
          () => document.querySelector('#ask-provider option[value="custom"]').disabled,
        );
        assert.ok(entryDisabled === true, "cleared-key entry is disabled");
      },
      { keepData: true },
    );

    // ------------------------------------------------------------------
    // Overwrite settings.json with a PARSEABLE but policy-invalid file,
    // then restart the app over the same data dir.
    // ------------------------------------------------------------------
    const settingsPath = path.join(firstDataDir, "com.acailic.mailchatvault", "settings.json");
    const invalid = {
      version: 1,
      providers: [
        {
          id: "evil",
          name: "",
          kind: "zai",
          base_url: "http://attacker.example",
          model: "",
        },
      ],
      default_provider: "evil",
      options: { theme: "dark", summarize_top: 8 },
    };
    await writeFile(settingsPath, JSON.stringify(invalid, null, 2) + "\n");

    await withSession(
      {
        dataDir: firstDataDir,
        env: { ZAI_API_KEY: "", GLM_API_KEY: "", ANTHROPIC_API_KEY: "", ANTHROPIC_BASE_URL: "" },
      },
      async (browser) => {
      await ui.waitForApp(browser);

      // The refusal is VISIBLE in the Settings screen.
      const problem = await ui.waitForText(
        browser,
        "#env-info",
        "Settings file problem",
        { timeoutMsg: "invalid settings.json never surfaced a visible error" },
      );
      assert.includes(problem, "z.ai", "the error names the violated pairing rule");

      // Nothing is enabled: local-only menu, off note — the evil provider
      // (which would route the stored key to attacker.example) never ran.
      const askProvider = await browser.$("#ask-provider");
      const options = await askProvider.$$("option");
      assert.equal(options.length, 1, "no provider from the refused file");
      assert.equal(await options[0].getValue(), "local");
      await ui.waitForText(browser, "#llm-note", "LLM assist is off");

      // And the app itself stays fully functional.
      const ask = await browser.$("#ask");
      await ask.click();
      await ask.setValue("vault?");
      await browser.keys("Enter");
      const plan = await ui.waitForText(
        browser,
        "#ask-status",
        (t) => t !== "" && !t.startsWith("Asking"),
        { timeoutMsg: "ask after refused settings never settled" },
      );
      assert.includes(plan, "local full-text", "ask degraded to local mode, no crash");
    });
  } finally {
    await mock.close();
  }
}
