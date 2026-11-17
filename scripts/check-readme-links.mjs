// Link check for README.md: every relative link target and heading
// anchor must resolve in the repository. External http(s) links and
// mailto: are skipped. Run: npm run check:docs
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const readmePath = join(repoRoot, "README.md");
const readme = readFileSync(readmePath, "utf8");

// Strip fenced code blocks so their contents never look like links.
const prose = readme.replace(/```[\s\S]*?```/g, "");

const targets = new Set();
for (const [, , target] of prose.matchAll(/(?:\[[^\]]*\]\(|\n\s*\[[^\]]+\]:\s*)(<?)([^)\s>]+)(>?)/g)) {
  targets.add(target);
}

/** GitHub-style heading slug (lowercase, strip punctuation, spaces to -, dedup). */
function headingAnchors(markdown) {
  const used = new Map();
  const anchors = new Set();
  for (const line of markdown.split("\n")) {
    const heading = line.match(/^#{1,6}\s+(.*)$/);
    if (!heading) continue;
    const base = heading[1]
      .trim()
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s-]/gu, "")
      .replace(/\s/g, "-");
    const seen = used.get(base) ?? 0;
    used.set(base, seen + 1);
    anchors.add(seen === 0 ? base : `${base}-${seen}`);
  }
  return anchors;
}

const failures = [];
for (const target of targets) {
  if (/^(https?:|mailto:)/.test(target)) continue;
  const [rawPath, anchor] = target.split("#", 2);
  let resolved = readmePath;
  if (rawPath) {
    resolved = join(repoRoot, decodeURIComponent(rawPath));
    if (!existsSync(resolved)) {
      failures.push(`missing file: ${target}`);
      continue;
    }
  }
  if (anchor) {
    const doc = rawPath ? readFileSync(resolved, "utf8") : readme;
    if (!headingAnchors(doc).has(anchor)) {
      failures.push(`missing anchor: ${target}`);
    }
  }
}

if (failures.length > 0) {
  console.error(`README link check failed (${failures.length}):`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log(`README link check passed (${targets.size} link target(s) checked).`);
