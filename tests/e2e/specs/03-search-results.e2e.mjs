// Path: full-text search with results — ranking, authors, snippets,
// encoded bodies, and hostile query input that must never be an error.

export const name = "search-results";

export async function run(ctx) {
  const { withSession, assert, ui } = ctx;

  await withSession({}, async (browser) => {
    await ui.waitForApp(browser);
    await ui.importMbox(browser, ui.archiveMbox);
    const mode = await ui.uiMode(browser);

    // "vault" matches the four vault mails across the midnight boundary.
    const vaultStatus = await ui.search(browser, "vault");
    assert.ok(
      vaultStatus.includes("for “vault”"),
      `status references the query, got: ${vaultStatus}`,
    );
    assert.equal(ui.totalFromStatus(vaultStatus, mode), 4, "vault match count");
    const vaultRows = await ui.resultTexts(browser);
    assert.equal(vaultRows.length, 4, "four vault rows rendered");
    for (const row of vaultRows) {
      assert.ok(
        row.toLowerCase().includes("vault"),
        `every vault row mentions vault: ${row.slice(0, 60)}…`,
      );
    }

    // Classic UI shows source/channel/chat in the meta line.
    if (mode === "classic") {
      assert.includes(vaultRows[0], "mbox/email/archive", "meta line shows source/channel/chat");
    }

    // Base64 transfer-encoded body was decoded before indexing.
    await ui.search(browser, "zephyr");
    const zephyrRows = await ui.resultTexts(browser);
    assert.equal(zephyrRows.length, 1, "one zephyr row");
    assert.includes(zephyrRows[0], "Encoded attachment note", "subject is searchable");
    assert.includes(zephyrRows[0], "hidden zephyr signal", "decoded body is searchable");

    // Author names are indexed (FTS author column).
    await ui.search(browser, "zdravko");
    assert.equal((await ui.resultTexts(browser)).length, 1, "author-only match");

    // Undated message: searchable, displayed with an unknown-time marker.
    await ui.search(browser, "amber");
    const amberRows = await ui.resultTexts(browser);
    assert.equal(amberRows.length, 1, "undated message is searchable");
    assert.includes(amberRows[0], "unknown time", "undated message shows unknown time");

    // No match is a clean zero, not an error.
    const missStatus = await ui.search(browser, "quartz");
    assert.equal(ui.totalFromStatus(missStatus, mode), 0, "absent term matches nothing");

    // Hostile input: quotes and FTS operators must be sanitized into plain
    // terms — a syntax error would surface as an error status.
    const hostileStatus = await ui.search(browser, 'vault" OR granite*');
    assert.ok(
      /results|of \d+/.test(hostileStatus),
      `hostile query produced a result status, got: ${hostileStatus}`,
    );
    const statusClass = (await (await browser.$("#status")).getAttribute("class")) ?? "";
    assert.ok(!statusClass.includes("error"), "hostile query is not an error state");
  });
}
