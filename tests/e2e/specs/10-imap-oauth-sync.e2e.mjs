// Path: Sync button semantics in Google sign-in (OAuth) mode — the
// round-5/round-6 review fixes: (1) an empty IMAP host passes the
// frontend gate in OAuth mode (the backend defaults it to
// imap.gmail.com, per the documented Gmail-only default); (2) a stale
// password left in the hidden password field is never sent in OAuth
// mode; (3) the password-mode frontend gate still requires a host.
// The real backend answers: the oauth branch errors with the
// "no Google sign-in stored" message, which proves the request reached
// the backend with auth=oauth (a password-mode request would instead
// resolve a stored password or fail with "host is required").
//
// The full token chain (mock OAuth → loopback redirect → mock IMAP
// XOAUTH2 → archive) lives in the vault-core integration suites
// (tests/oauth_flow.rs and tests/imap/).

import { mkdirSync } from "node:fs";

export const name = "imap-oauth-sync";

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

/** Clear a text input's value directly (WebKitWebDriver rejects setValue("")). */
async function clearInput(browser, selector) {
  await browser.execute((sel) => {
    const el = document.querySelector(sel);
    el.value = "";
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }, selector);
}

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  // One session: env configured (sign-in available in principle), no
  // refresh token stored — Sync in OAuth mode must reach the backend and
  // fail with the oauth-branch error, never the frontend host gate.
  await withSession(
    {
      env: {
        MAILCHAT_GMAIL_CLIENT_ID: "e2e-client.apps.googleusercontent.com",
        MAILCHAT_GMAIL_CLIENT_SECRET: "e2e-secret-GOCSPX",
      },
    },
    async (browser) => {
      await ui.waitForApp(browser);

      // Leave a stale password in the password field while in password
      // mode — this value must never reach the backend in OAuth mode.
      await (await browser.$("#imap-user")).setValue("you@gmail.com");
      await (await browser.$("#imap-pass")).setValue("stale-password-never-send");
      await pickAuthMode(browser, "oauth");
      assert.ok(
        !(await (await browser.$("#imap-pass")).isDisplayed()),
        "password field hidden in OAuth mode",
      );

      // The host ships pre-filled (imap.gmail.com); clear it so Sync
      // exercises the documented empty-host default path.
      await clearInput(browser, "#imap-host");

      // Sync in OAuth mode with an empty host and a stale hidden
      // password: the frontend gate lets it through, the backend takes
      // the oauth branch (empty host → imap.gmail.com default) and fails
      // only because no refresh token is stored for the account.
      await (await browser.$("#imap-sync-btn")).click();
      const status = await ui.waitForText(
        browser,
        "#imap-status",
        "no Google sign-in stored",
        {
          timeoutMsg:
            "oauth-mode sync with empty host never reached the backend oauth branch",
        },
      );
      assert.ok(
        status.includes("Sign in with Google"),
        `status invites the sign-in, got: ${status}`,
      );
      await shot(browser, "10-oauth-sync-empty-host-reaches-backend");

      // Adversarial: in password mode the same empty host is still
      // rejected by the frontend gate (the Gmail default is OAuth-only).
      await pickAuthMode(browser, "password");
      assert.ok(
        await (await browser.$("#imap-pass")).isDisplayed(),
        "password field visible again in password mode",
      );
      await (await browser.$("#imap-sync-btn")).click();
      const gate = await ui.waitForText(
        browser,
        "#imap-status",
        "Enter the IMAP host and your username first.",
        {
          timeoutMsg:
            "password-mode sync with empty host was not stopped by the frontend gate",
        },
      );
      assert.ok(
        !gate.includes("Sync failed"),
        `the gate message is frontend-local, no backend error: ${gate}`,
      );
      await shot(browser, "10-password-mode-empty-host-gated");
    },
  );
}
