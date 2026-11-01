import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { formatSentAt, snippet } from "./format";
import { describePlan, privacyNote, type AskOutcome, type LlmStatus } from "./llmview";

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

interface FolderSyncSummary {
  folder: string;
  uidvalidity: number;
  reset: boolean;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
  /** Why the folder failed to sync; null on success. */
  error: string | null;
}

interface ImapSyncOutcome {
  folders: FolderSyncSummary[];
  /** null when a stored password was reused; "os_keyring" | "memory" when
   * the entered password was just saved there. */
  saved_to: "os_keyring" | "memory" | null;
}

const searchInput = document.querySelector<HTMLInputElement>("#search")!;
const searchButton = document.querySelector<HTMLButtonElement>("#search-btn")!;
const searchStatus = document.querySelector<HTMLElement>("#status")!;
const resultsList = document.querySelector<HTMLUListElement>("#results")!;
const statsLabel = document.querySelector<HTMLElement>("#stats")!;
const mboxPathInput = document.querySelector<HTMLInputElement>("#mbox-path")!;
const importButton = document.querySelector<HTMLButtonElement>("#import-btn")!;
const chatImportButton = document.querySelector<HTMLButtonElement>("#chat-import-btn")!;
const seedButton = document.querySelector<HTMLButtonElement>("#seed-btn")!;
const importStatus = document.querySelector<HTMLElement>("#import-status")!;
const askInput = document.querySelector<HTMLInputElement>("#ask")!;
const askButton = document.querySelector<HTMLButtonElement>("#ask-btn")!;
const askStatus = document.querySelector<HTMLElement>("#ask-status")!;
const llmNote = document.querySelector<HTMLElement>("#llm-note")!;
const answerBox = document.querySelector<HTMLElement>("#answer")!;
const askNotes = document.querySelector<HTMLElement>("#ask-notes")!;
const askResults = document.querySelector<HTMLUListElement>("#ask-results")!;
const imapHostInput = document.querySelector<HTMLInputElement>("#imap-host")!;
const imapPortInput = document.querySelector<HTMLInputElement>("#imap-port")!;
const imapUserInput = document.querySelector<HTMLInputElement>("#imap-user")!;
const imapPassInput = document.querySelector<HTMLInputElement>("#imap-pass")!;
const imapFoldersInput = document.querySelector<HTMLInputElement>("#imap-folders")!;
const imapSyncButton = document.querySelector<HTMLButtonElement>("#imap-sync-btn")!;
const imapStatus = document.querySelector<HTMLElement>("#imap-status")!;

function setStatus(text: string, isError = false): void {
  searchStatus.textContent = text;
  searchStatus.classList.toggle("error", isError);
}

function renderResults(list: HTMLUListElement, messages: Message[]): void {
  list.replaceChildren(
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
    renderResults(resultsList, messages);
  } catch (e) {
    setStatus(String(e), true);
  }
}

function renderAsk(outcome: AskOutcome): void {
  if (outcome.answer) {
    answerBox.textContent = outcome.answer;
    answerBox.hidden = false;
  } else {
    answerBox.hidden = true;
  }
  askNotes.textContent = outcome.notes.join(" · ");
  askNotes.hidden = outcome.notes.length === 0;
  const shown = outcome.results as Message[];
  renderResults(askResults, shown);
}

async function runAsk(): Promise<void> {
  const question = askInput.value.trim();
  if (!question) {
    askStatus.textContent = "Ask a question first.";
    return;
  }
  askButton.disabled = true;
  askStatus.textContent = "Asking… (translate → search → summarize)";
  try {
    const outcome = await invoke<AskOutcome>("llm_ask", { question, limit: 50 });
    askStatus.textContent = describePlan(outcome);
    renderAsk(outcome);
  } catch (e) {
    askStatus.textContent = String(e);
  } finally {
    askButton.disabled = false;
  }
}

async function refreshLlmNote(): Promise<void> {
  try {
    const status = await invoke<LlmStatus>("llm_status");
    llmNote.textContent = privacyNote(status);
  } catch {
    llmNote.textContent = "";
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

async function importChatExport(): Promise<void> {
  chatImportButton.disabled = true;
  try {
    const picked = await open({
      multiple: false,
      title: "Import chat export",
      filters: [
        {
          name: "Chat export (WhatsApp .txt, Telegram result.json)",
          extensions: ["txt", "json"],
        },
      ],
    });
    if (typeof picked !== "string") {
      return; // cancelled
    }
    importStatus.textContent = `Importing ${picked}…`;
    const summary = await invoke<IngestSummary>("import_chat_export", { path: picked });
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).`;
    await refreshStats();
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    chatImportButton.disabled = false;
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

async function syncImap(): Promise<void> {
  const host = imapHostInput.value.trim();
  const username = imapUserInput.value.trim();
  const port = Number.parseInt(imapPortInput.value, 10);
  const password = imapPassInput.value;
  const folders = imapFoldersInput.value
    .split(",")
    .map((f) => f.trim())
    .filter((f) => f.length > 0);
  if (!host || !username) {
    imapStatus.textContent = "Enter the IMAP host and your username first.";
    return;
  }
  imapSyncButton.disabled = true;
  imapStatus.textContent = `Syncing ${username} over IMAP… (first sync of a large mailbox can take a while)`;
  try {
    const outcome = await invoke<ImapSyncOutcome>("sync_imap", {
      host,
      port: Number.isNaN(port) ? null : port,
      username,
      password,
      folders: folders.length > 0 ? folders : null,
    });
    const lines = outcome.folders.map(
      (f) =>
        `${f.folder}: ${f.inserted} new, ${f.skipped} already present` +
        (f.failed > 0 ? `, ${f.failed} failed to parse` : "") +
        (f.reset ? " (full rescan)" : "") +
        (f.error ? ` — failed: ${f.error}` : ""),
    );
    const note =
      outcome.saved_to === "memory"
        ? " Password kept in memory only for this session (no OS keyring available)."
        : outcome.saved_to === "os_keyring"
          ? " Password stored in the OS keyring."
          : "";
    imapStatus.textContent = lines.join(" · ") + note;
    imapPassInput.value = "";
    await refreshStats();
  } catch (e) {
    imapStatus.textContent = `Sync failed: ${e}`;
  } finally {
    imapSyncButton.disabled = false;
  }
}

searchButton.addEventListener("click", () => void runSearch());
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch();
});
askButton.addEventListener("click", () => void runAsk());
askInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runAsk();
});
importButton.addEventListener("click", () => void importMbox());
chatImportButton.addEventListener("click", () => void importChatExport());
seedButton.addEventListener("click", () => void seedDemo());
imapSyncButton.addEventListener("click", () => void syncImap());

void refreshStats();
void refreshLlmNote();
