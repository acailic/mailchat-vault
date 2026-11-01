import { invoke } from "@tauri-apps/api/core";
import { formatSentAt, snippet } from "./format";

interface Message {
  id: number;
  source: string;
  external_id: string;
  channel: string;
  chat_id: string;
  author: string;
  sent_at: string;
  body: string;
  raw_json: string;
}

interface SourceCount {
  source: string;
  count: number;
}

interface Stats {
  total: number;
  by_source: SourceCount[];
}

interface IngestSummary {
  source: string;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
}

const searchInput = document.querySelector<HTMLInputElement>("#search")!;
const searchButton = document.querySelector<HTMLButtonElement>("#search-btn")!;
const searchStatus = document.querySelector<HTMLElement>("#status")!;
const resultsList = document.querySelector<HTMLUListElement>("#results")!;
const statsLabel = document.querySelector<HTMLElement>("#stats")!;
const mboxPathInput = document.querySelector<HTMLInputElement>("#mbox-path")!;
const importButton = document.querySelector<HTMLButtonElement>("#import-btn")!;
const seedButton = document.querySelector<HTMLButtonElement>("#seed-btn")!;
const importStatus = document.querySelector<HTMLElement>("#import-status")!;

function setStatus(text: string, isError = false): void {
  searchStatus.textContent = text;
  searchStatus.classList.toggle("error", isError);
}

function renderResults(messages: Message[]): void {
  resultsList.replaceChildren(
    ...messages.map((m) => {
      const li = document.createElement("li");
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `${m.author || "unknown"} · ${formatSentAt(m.sent_at)} · ${m.source}/${m.channel}${m.chat_id ? "/" + m.chat_id : ""}`;
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = snippet(m.body);
      li.append(meta, body);
      return li;
    }),
  );
}

async function runSearch(): Promise<void> {
  const query = searchInput.value.trim();
  if (!query) {
    setStatus("Type something to search.");
    return;
  }
  setStatus("Searching…");
  try {
    const messages = await invoke<Message[]>("search_messages", { query, limit: 50 });
    setStatus(`${messages.length} result${messages.length === 1 ? "" : "s"} for “${query}”`);
    renderResults(messages);
  } catch (e) {
    setStatus(String(e), true);
  }
}

async function refreshStats(): Promise<void> {
  try {
    const stats = await invoke<Stats>("stats");
    const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
    statsLabel.textContent = stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
  } catch {
    statsLabel.textContent = "";
  }
}

async function importMbox(): Promise<void> {
  const path = mboxPathInput.value.trim();
  if (!path) {
    importStatus.textContent = "Enter a path to an .mbox file first.";
    return;
  }
  importButton.disabled = true;
  importStatus.textContent = `Importing ${path}…`;
  try {
    const summary = await invoke<IngestSummary>("import_mbox", { path });
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).`;
    await refreshStats();
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    importButton.disabled = false;
  }
}

async function seedDemo(): Promise<void> {
  seedButton.disabled = true;
  try {
    const summary = await invoke<IngestSummary>("seed_demo");
    importStatus.textContent = `Demo messages: ${summary.inserted} new, ${summary.skipped} already present.`;
    await refreshStats();
  } catch (e) {
    importStatus.textContent = `Seeding failed: ${e}`;
  } finally {
    seedButton.disabled = false;
  }
}

searchButton.addEventListener("click", () => void runSearch());
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch();
});
importButton.addEventListener("click", () => void importMbox());
seedButton.addEventListener("click", () => void seedDemo());

void refreshStats();
