// Path: Gmail Google sign-in (OAuth2/XOAUTH2) in the IMAP sync section —
// the auth-mode picker, the real oauth_status backend command, and its UI
// states. No Google credentials are involved: the interesting real paths
// are exactly the unconfigured / not-signed-in ones, asserted against the
// live Rust backend. The sign-in itself (token flow incl. the browser-open
// failure) is covered hermetically in the vault-core integration suites
// (tests/oauth_flow.rs), so this spec never depends on the host having —
// or lacking — a browser opener.

import { mkdirSync } from "node:fs";

export const name = "imap-oauth";

/** Save a proof screenshot inside the container (copied out by the runner). */
async function shot(browser, name) {
  const dir = process.env.MAILCHAT_E2E_SHOTS ?? "/tmp/e2e-shots";
  try {
    mkdirSync(dir, { recursive: true });
    await browser.saveScreenshot(`${dir}/${name}.png`);
  } catch {
    // Screenshots are a bonus, never a failure cause.
  }
}

/** Pick the auth mode like a user would (value + change event). */
async function pickAuthMode(browser, value) {
  await browser.execute(
    (val) => {
      const el = document.querySelector("#imap-auth");
      el.value = val;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    },
    value,
  );
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  // Session 1: no OAuth env vars — the sign-in is explicitly off.
  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);

    // Default mode is app password: the password field shows, the Google
    // sign-in button does not.
    assert.ok(await (await browser.$("#imap-auth")).isExisting(), "auth picker rendered");
    assert.equal(
      await (await browser.$("#imap-auth")).getValue(),
      "password",
      "default auth mode is app password",
    );
    assert.ok(await (await browser.$("#imap-pass")).isDisplayed(), "password field visible");
    assert.ok(!(await (await browser.$("#imap-oauth-btn")).isDisplayed()), "sign-in button hidden in password mode");

    // Switch to Google sign-in: the password field disappears, the
    // sign-in button appears, and the real backend reports the missing
    // configuration (no env vars in this session).
    await (await browser.$("#imap-user")).setValue("you@gmail.com");
    await pickAuthMode(browser, "oauth");
    assert.ok(!(await (await browser.$("#imap-pass")).isDisplayed()), "password field hidden in OAuth mode");
    assert.ok(
      await (await browser.$("#imap-oauth-btn")).isDisplayed(),
      "sign-in button visible in OAuth mode",
    );
    const hint = await ui.waitForText(
      browser,
      "#imap-oauth-hint",
      "Gmail sign-in is not configured",
      { timeoutMsg: "unconfigured hint never appeared" },
    );
    assert.ok(
      hint.includes("MAILCHAT_GMAIL_CLIENT_ID"),
      `hint names the env var to set, got: ${hint}`,
    );
    await shot(browser, "09-oauth-unconfigured");

    // Signing in anyway surfaces the real backend error and re-enables.
    await (await browser.$("#imap-oauth-btn")).click();
    const status = await ui.waitForText(browser, "#imap-status", "Sign-in failed", {
      timeoutMsg: "sign-in failure status never appeared",
    });
    assert.ok(
      status.includes("Gmail sign-in is not configured"),
      `status carries the backend error, got: ${status}`,
    );
    assert.ok(await (await browser.$("#imap-oauth-btn")).isEnabled(), "button re-enabled");

    // Switching back restores the password mode UI.
    await pickAuthMode(browser, "password");
    assert.ok(
      await (await browser.$("#imap-pass")).isDisplayed(),
      "password field visible again",
    );
    assert.ok(!(await (await browser.$("#imap-oauth-btn")).isDisplayed()), "sign-in button hidden again");
  });

  // Session 2: env vars set (the user's own client), still not signed in —
  // the hint reflects the real keyring-backed oauth_status. The sign-in
  // itself is deliberately not clicked here: with the client configured it
  // runs the real browser flow, which blocks until the host's browser
  // opener succeeds or times out — an environment-dependent outcome. The
  // full flow incl. the browser-open failure is covered hermetically in
  // tests/oauth_flow.rs (mock OAuth server + injected openers).
  await withSession(
    {
      env: {
        MAILCHAT_GMAIL_CLIENT_ID: "e2e-client.apps.googleusercontent.com",
        MAILCHAT_GMAIL_CLIENT_SECRET: "e2e-secret",
      },
    },
    async (browser) => {
      await ui.waitForApp(browser);
      await (await browser.$("#imap-user")).setValue("you@gmail.com");
      await pickAuthMode(browser, "oauth");
      const hint = await ui.waitForText(
        browser,
        "#imap-oauth-hint",
        "Not signed in yet",
        { timeoutMsg: "configured-but-not-signed-in hint never appeared" },
      );
      assert.ok(hint.includes("Sign in with Google"), `hint invites the sign-in, got: ${hint}`);
      assert.ok(
        await (await browser.$("#imap-oauth-btn")).isDisplayed(),
        "sign-in button visible when configured",
      );
    },
  );
}
