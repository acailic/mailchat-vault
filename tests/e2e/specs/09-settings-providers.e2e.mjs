// Path: Settings UI + multi-provider Ask menu. Configures a provider
// through the Settings screen (never environment variables), proves the
// settings file on disk never contains the key, the key travels to the
// mock endpoint exactly once per ask, per-ask local mode sends nothing,
// and the pairing rules refuse a mis-paired provider before saving.

import { readFile } from "node:fs/promises";
import path from "node:path";

export const name = "settings-providers";

const TRANSLATE = JSON.stringify({ match: '"vault"', date_from: null, date_to: null });

/** Set a <select> and fire its change event (tauri-driver's WebKit session
 * does not implement the JSONWire select helpers). */
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

export async function run(ctx) {
  const { withSession, assert, ui, startMockLlm } = ctx;

  const mock = await startMockLlm([TRANSLATE]);

  try {
    await withSession(
      {
        env: {
          // This spec must start from a clean slate: whatever LLM variables
          // the surrounding environment carries must not leak in.
          ZAI_API_KEY: "",
          GLM_API_KEY: "",
          ANTHROPIC_API_KEY: "",
          ANTHROPIC_BASE_URL: "",
          MAILCHAT_LLM_MODEL: "",
        },
      },
      async (browser, { dataDir }) => {
      await ui.waitForApp(browser);

      if (!(await browser.$("#settings").isExisting())) {
        ctx.skip("Settings UI not present in this build");
      }

      // --- fresh install: local-only menu, off note -------------------------
      await ui.openView(browser, "workspace");
      const askProvider = await browser.$("#ask-provider");
      let options = await askProvider.$$("option");
      assert.equal(options.length, 1, "fresh install lists only local mode");
      assert.equal(await options[0].getText(), "Local full-text (no LLM)");
      await ui.waitForText(browser, "#llm-note", "LLM assist is off");

      // --- configure a custom provider through Settings ---------------------
      await ui.openView(browser, "settings");
      await setSelect(browser, "#add-kind", "custom");
      await browser.$("#add-provider-btn").click();
      const row = await browser.$('.provider-row[data-id="custom"]');
      await row.$(".prov-name").setValue("Mock Lab");
      await row.$(".prov-url").setValue(`http://127.0.0.1:${mock.port}/api`);
      await row.$(".prov-model").setValue("settings-model");
      await row.$(".prov-key").setValue("settings-key");
      await row.$('input[type="radio"]').click(); // make it the default
      await setSelect(browser, "#theme", "dark");

      await browser.$("#settings-save-btn").click();
      await ui.waitForText(browser, "#settings-note", "Saved.");
      assert.ok(
        /(OS keyring|session memory)/.test(await browser.$("#settings-note").getText()),
        "save note says where the key was kept",
      );
      // Theme option applied through the settings file round trip.
      assert.equal(
        await browser.$("#theme").getValue(),
        "dark",
        "theme select reflects the saved option",
      );

      // --- the settings file holds the profile but never the key -----------
      const settingsJson = JSON.parse(
        await readFile(
          path.join(dataDir, "com.acailic.mailchatvault", "settings.json"),
          "utf8",
        ),
      );
      assert.equal(settingsJson.providers.length, 1, "one provider persisted");
      const provider = settingsJson.providers[0];
      assert.equal(provider.id, "custom");
      assert.equal(provider.name, "Mock Lab");
      assert.equal(provider.model, "settings-model");
      assert.equal(settingsJson.default_provider, "custom");
      assert.equal(settingsJson.options.theme, "dark");
      const raw = JSON.stringify(settingsJson);
      assert.ok(!raw.includes("settings-key"), "settings file never contains the API key");
      assert.ok(!raw.includes("api_key"), "no api_key field exists in the file format");

      // --- the Ask menu now offers the provider and preselects it -----------
      await ui.openView(browser, "workspace");
      options = await askProvider.$$("option");
      assert.equal(options.length, 2, "local + the configured provider");
      assert.equal(await askProvider.getValue(), "custom", "default preselected");
      assert.ok(
        (await options[1].getText()).includes("Mock Lab (settings-model · 127.0.0.1)"),
        "menu entry names the provider, model and host",
      );
      const note = await ui.waitForText(browser, "#llm-note", "LLM assist is on");
      assert.includes(note, "settings-model via 127.0.0.1", "note names model and host");

      // --- ask with the provider: exactly what leaves the machine ----------
      const askInput = await browser.$("#ask");
      await askInput.click();
      await askInput.setValue("anything about the vault?");
      await browser.keys("Enter");
      const plan = await ui.waitForText(
        browser,
        "#ask-status",
        (text) => text !== "" && !text.startsWith("Asking"),
        { timeoutMsg: "settings-provider ask never settled" },
      );
      assert.includes(plan, "planned by LLM", "ask used the configured provider");
      assert.equal(mock.requests.length, 1, "one translation round trip, no results to summarize");
      assert.equal(mock.requests[0].body.model, "settings-model", "model from settings");
      assert.equal(
        mock.requests[0].headers["x-api-key"],
        "settings-key",
        "key from the key store, sent to the configured endpoint only",
      );

      // --- per-ask local mode: nothing leaves -------------------------------
      await setSelect(browser, "#ask-provider", "local");
      await ui.waitForText(browser, "#llm-note", "LLM assist is off");
      await askInput.click();
      await askInput.setValue("vault again, but locally");
      await browser.keys("Enter");
      const localPlan = await ui.waitForText(
        browser,
        "#ask-status",
        (text) => text !== "" && !text.startsWith("Asking"),
        { timeoutMsg: "local ask never settled" },
      );
      assert.includes(
        localPlan,
        "local full-text — nothing sent to any provider",
        "local ask is explicit local mode",
      );
      assert.equal(
        mock.requests.length,
        1,
        "local mode sends nothing to any provider",
      );

      // --- pairing rules refuse a mis-paired provider before saving --------
      await ui.openView(browser, "settings");
      await setSelect(browser, "#add-kind", "zai");
      await browser.$("#add-provider-btn").click();
      const zaiRow = await browser.$('.provider-row[data-id="zai"]');
      // A z.ai provider pointed at the Anthropic host must be refused.
      await zaiRow.$(".prov-url").setValue("https://api.anthropic.com");
      await browser.$("#settings-save-btn").click();
      const refused = await ui.waitForText(browser, "#settings-note", "Not saved");
      assert.includes(
        refused,
        "only ever sent to the z.ai endpoint",
        "save refused with the pairing error",
      );
      // The refused save changed nothing: the file still has one provider.
      const afterRefusal = JSON.parse(
        await readFile(
          path.join(dataDir, "com.acailic.mailchatvault", "settings.json"),
          "utf8",
        ),
      );
      assert.equal(
        afterRefusal.providers.length,
        1,
        "refused save leaves the settings file untouched",
      );

      // --- clearing the stored key disables the provider menu entry --------
      await browser.$('.provider-row[data-id="custom"] .clear-key-btn').click();
      await ui.openView(browser, "workspace");
      const customOption = await askProvider.$('option[value="custom"]');
      await browser.waitUntil(
        async () => (await customOption.getText()).includes("no API key"),
        { timeout: 15_000, timeoutMsg: "menu entry never reflected the cleared key" },
      );
    });
  } finally {
    await mock.close();
  }
}
