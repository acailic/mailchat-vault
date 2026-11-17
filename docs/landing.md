<!doctype html>
<!-- Mailchat Vault landing page - public launch copy (deploy-ready).
     One self-contained file: no external assets, no scripts, no tracking.
     Owner action: deploy as-is (e.g. save as index.html on any static host).
     Keep the promise, the comparison table, and the single CTA in sync with
     the README front door. Strategy and monetization material stays outside
     the repository (see docs/README.md). -->
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="description" content="Mailchat Vault - one private, searchable archive for your email and chats, fully offline on your own machine. Free and open source.">
<title>Mailchat Vault — your email and chats, one private archive on your machine</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
    line-height: 1.6;
    background: #f6f5f4;
    color: #1c1917;
  }
  main { max-width: 72rem; margin: 0 auto; padding: 3rem 1.25rem 4rem; }
  h1 { font-size: 2.2rem; line-height: 1.2; margin: 0 0 1rem; }
  .promise { font-size: 1.25rem; max-width: 46rem; margin: 0 0 1.75rem; }
  .cta {
    display: inline-block;
    background: #1d4ed8;
    color: #fff;
    text-decoration: none;
    font-weight: 600;
    font-size: 1.1rem;
    padding: 0.8rem 1.6rem;
    border-radius: 10px;
    margin-bottom: 0.5rem;
  }
  .cta:hover { background: #1e40af; }
  .cta-note { font-size: 0.95rem; color: #57534e; margin: 0 0 2.5rem; }
  h2 { font-size: 1.4rem; margin: 2.5rem 0 0.75rem; }
  .table-wrap { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; min-width: 52rem; background: #fff; }
  th, td { border: 1px solid #d6d3d1; padding: 0.55rem 0.7rem; text-align: left; font-size: 0.92rem; vertical-align: top; }
  thead th { background: #e7e5e4; font-size: 0.9rem; }
  tbody th { background: #fafaf9; white-space: nowrap; }
  tbody tr :first-child { font-weight: 600; }
  .col-us { background: #eff6ff; }
  .fineprint { font-size: 0.9rem; color: #57534e; max-width: 46rem; }
  footer {
    border-top: 1px solid #d6d3d1;
    margin-top: 3rem;
    padding-top: 1.25rem;
    font-size: 0.9rem;
    color: #57534e;
  }
  footer a { color: #1d4ed8; }
  @media (prefers-color-scheme: dark) {
    body { background: #1c1917; color: #f5f5f4; }
    table { background: #292524; }
    th, td { border-color: #57534e; }
    thead th { background: #44403c; }
    tbody th { background: #33302e; }
    .col-us { background: #1e3a5f; }
    .cta-note, .fineprint, footer { color: #a8a29e; }
  }
</style>
</head>
<body>
<main>
  <h1>Mailchat&nbsp;Vault</h1>
  <p class="promise">
    One private, searchable archive for your email and your chats, on your own
    machine: drop in a Gmail Takeout export or a WhatsApp, Telegram, Signal,
    Discord, or Messenger export and find any message in seconds — fully
    offline, with no cloud service holding your messages.
  </p>
  <a class="cta" href="https://github.com/acailic/mailchat-vault/releases/latest">Download for macOS, Windows, or Linux</a>
  <p class="cta-note">Free and open source (GPL-3.0-only). Linux, macOS, and Windows installers with checksums.</p>

  <h2>How it compares</h2>
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th scope="col"></th>
          <th scope="col" class="col-us">Mailchat Vault</th>
          <th scope="col">MailStore Home<br>(Windows)</th>
          <th scope="col">Mail Archiver X<br>(macOS)</th>
          <th scope="col">MailSteward<br>(macOS)</th>
          <th scope="col">Thunderbird +<br>ImportExportTools NG</th>
          <th scope="col">Gmail search</th>
          <th scope="col">AI Emaily</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row">Offline, local archive</th>
          <td class="col-us">Yes — archive and search work fully offline</td>
          <td>Yes — local archive</td>
          <td>Yes — local archive</td>
          <td>Yes — local archive (Pro on a local MySQL server)</td>
          <td>Yes — local mail folders</td>
          <td>No — mail stays on Google's servers</td>
          <td>No — cloud service</td>
        </tr>
        <tr>
          <th scope="row">Email + chat in one archive</th>
          <td class="col-us">Yes — <code>.mbox</code>/<code>.eml</code> and IMAP/Gmail plus WhatsApp, Telegram, Signal, Discord, Messenger</td>
          <td>Email only</td>
          <td>Email only</td>
          <td>Email only</td>
          <td>Email only</td>
          <td>Email only</td>
          <td>Email only</td>
        </tr>
        <tr>
          <th scope="row">AI search that never leaves the machine</th>
          <td class="col-us">Hybrid ranking with local embeddings runs offline (no model downloads); the optional AI ask is consent-gated and sends only what the prompt shows</td>
          <td>No AI</td>
          <td>No AI</td>
          <td>No AI</td>
          <td>No AI</td>
          <td>AI (Gemini) runs in the cloud</td>
          <td>AI runs in the cloud</td>
        </tr>
        <tr>
          <th scope="row">Court-ready export</th>
          <td class="col-us">JSON Lines export keeps per-message provenance — source, resolved time + confidence, content hash, import receipt</td>
          <td>No</td>
          <td>Not stated by vendor</td>
          <td>Markets FRCP compliance; mbox/SQL/text export</td>
          <td>PDF/HTML/CSV export, no provenance</td>
          <td>No</td>
          <td>No</td>
        </tr>
        <tr>
          <th scope="row">Open formats (mbox/eml export)</th>
          <td class="col-us">MBOX export (mail), CSV, JSON Lines round-trip; <code>.mbox</code>/<code>.eml</code> import</td>
          <td>Restore-oriented export from its own archive format; no open export format documented</td>
          <td>Not stated by vendor</td>
          <td>mbox, SQL, tab-delimited text</td>
          <td>EML, MBOX, PDF, CSV, plaintext</td>
          <td>Google Takeout (<code>.mbox</code>) only</td>
          <td>Not stated by vendor</td>
        </tr>
        <tr>
          <th scope="row">Price</th>
          <td class="col-us">Free — open source, GPL-3.0-only</td>
          <td>Free (private use)</td>
          <td>$50</td>
          <td>$24.95–$99.95 (Pro needs MySQL)</td>
          <td>Free</td>
          <td>Free (Google account)</td>
          <td>From $14.99/month, no free plan</td>
        </tr>
      </tbody>
    </table>
  </div>
  <p class="fineprint">
    Competitor facts checked against vendor pages, 2026-11. Where the network
    is involved: the archive and search stay on your machine and search works
    fully offline; connecting an account contacts your mail provider (IMAP
    sync is read-only), and the optional AI ask sends only what its explicit
    consent prompt shows. The app does not encrypt the archive or backups
    itself — at rest they rely on your OS account and disk protection.
  </p>

  <footer>
    Mailchat Vault is free software under GPL-3.0-only —
    <a href="https://github.com/acailic/mailchat-vault">source and documentation on GitHub</a>.
  </footer>
</main>
</body>
</html>
