# Product review evidence — 2026-11-06

Application source: `cb8b67d86fda8929130b452dcddc1c5a97850dff`, version 0.2.0. Environment: Pop!_OS 24.04, native Tauri/WebKitGTK desktop application. The local development binary embedded the built frontend; this was not a published-installer usability test. Product conclusions are in the [review](../../internal/product-review.md).

## Files

| File | Origin and meaning |
| --- | --- |
| [first-run.png](first-run.png) | Fresh temporary archive, no AI credentials, before import or any Ask submission |
| [add-source.png](add-source.png) | The same clean app's Add source view; illustrates typed mbox path and technical mail settings |
| [reading.png](reading.png) | Synthetic demo seed, search for `vault`, first result opened |
| [settings.png](settings.png) | Settings after demo seed; no provider credentials configured |
| [compact.png](compact.png) | Earlier same-session native walkthrough at 640 pixels, synthetic reading pane |
| [observations.json](observations.json) | Live native-webview text and computed-state observations, including the visible hidden consent row and unconfigured Gmail setup hint |
| [layout-observations.json](layout-observations.json) | Earlier walkthrough's actual 640/900/1280-pixel layout and horizontal-clipping observations; local directory metadata excluded |
| [smoke-results.json](smoke-results.json) | Results of eight existing E2E paths against the installed binary in the project's Linux E2E container, using fresh per-test archives |

The screenshots contain repository-defined demo messages only. The source configuration check used a synthetic address and did not initiate login or sync. `document.fonts` confirmed the bundled Source Sans 3 was loaded in the fresh walkthrough. Do not infer a font-loading defect from an image's appearance alone.

## Reproduction

Follow the prerequisites and runner setup in the [E2E guide](../../../tests/e2e/README.md). Build the frontend and native application with embedded assets as described there, or select a known binary using `MAILCHAT_E2E_BINARY`.

The recorded existing E2E specifications are `01-empty-archive`, `02-import-mbox`, `03-search-results`, `04-keyboard-nav`, `07-detail-view`, `09-settings-providers`, `11-stats-dashboard`, and `16-search-v2`. The normal runner supports name filters such as `MAILCHAT_E2E_ONLY=empty-archive npm run test:e2e`; see the guide for display/driver requirements. Each test uses an isolated archive; the Settings provider path uses a local mock endpoint.

For the fresh inspection, launch a new temporary data directory, open each destination through the navigation, seed demo data only after capturing first run and Add source, search `vault`, and open the first result. To reproduce the consent observation, inspect `#consent-prompt.hidden`, its computed display and button bounds before submitting a question. The observed values were `true`, `flex`, and nonzero button widths. Clear provider environment variables for this inspection; do not print their values.

For layout observations, resize to 900, 640 and 1280 pixels and record `#app`'s `data-layout` plus `document.documentElement.scrollWidth > document.documentElement.clientWidth + 1`. The inspected states were medium, compact and wide, with no horizontal clipping in those views.

## Limits

The JSON test durations are whole-spec elapsed times, not search latency measurements. These eight paths and the fresh walkthrough do not constitute the full regression, resilience, accessibility, security or three-platform release matrix. No live mail account was connected; no backup/restore was verified; no customer participated. Screenshots establish observed UI state, not customer behavior or market demand.
