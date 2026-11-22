// Positioning single-source check: the canonical block in
// docs/landing.md must be quoted byte for byte by the README front door
// and the Product Hunt first comment, the README promise must be the
// block's one-liner, the landing page must render the same sentence and
// carry social-card meta, and the listing tagline must fit Product
// Hunt's 60-character cap. Run: npm run check:docs
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path) => readFileSync(join(repoRoot, path), "utf8");

const START = "positioning-block:start";
const END = "positioning-block:end";

/** Text between the marker lines of a positioning block (both the
 *  single-comment canonical form and the two-comment quote form). */
function extractPositioningBlock(text, file) {
  const start = text.indexOf(START);
  const end = text.indexOf(END);
  if (start === -1 || end === -1 || end < start) {
    throw new Error(`${file}: ${START} / ${END} markers missing or out of order`);
  }
  const contentStart = text.indexOf("\n", start) + 1;
  const contentEnd = text.lastIndexOf("\n", end) + 1;
  return text.slice(contentStart, contentEnd);
}

const failures = [];

const landing = read("docs/landing.md");
const canonical = extractPositioningBlock(landing, "docs/landing.md");
const oneLiner = canonical.slice(0, canonical.indexOf("\n"));

for (const file of ["README.md", "docs/launch/product-hunt/first-comment.md"]) {
  const quoted = extractPositioningBlock(read(file), file);
  if (quoted !== canonical) {
    failures.push(`${file}: positioning block does not match docs/landing.md byte for byte`);
  }
}

// The README front-door promise sentence is the positioning one-liner.
const readme = read("README.md");
const firstContentLine = readme
  .replace(/<!--[\s\S]*?-->/g, "")
  .split("\n")
  .map((line) => line.trim())
  .find((line) => line.length > 0 && !line.startsWith("#"));
if (firstContentLine !== oneLiner) {
  failures.push("README.md: the front-door promise sentence is not the positioning one-liner");
}

// The landing page renders the same promise (HTML-wrapped, so compare
// whitespace-normalized) and carries the social-card meta.
const flat = (text) => text.replace(/\s+/g, " ").trim();
const promise = landing.match(/<p class="promise">([\s\S]*?)<\/p>/);
if (!promise || flat(promise[1]) !== flat(oneLiner)) {
  failures.push("docs/landing.md: displayed promise differs from the positioning one-liner");
}
for (const meta of ['property="og:title"', 'property="og:description"', 'property="og:image"', 'name="twitter:card" content="summary_large_image"']) {
  if (!landing.includes(meta)) {
    failures.push(`docs/landing.md: missing social-card meta (${meta})`);
  }
}

// The privacy claim keeps its precise wording.
if (!canonical.includes("search stays on this device")) {
  failures.push("canonical positioning block: privacy claim wording 'search stays on this device' is missing");
}

// The listing tagline fits Product Hunt's 60-character cap.
const tagline = read("docs/launch/product-hunt/listing.md").match(/\*\*Tagline:\*\*([^\n]+)/);
const taglineText = tagline ? tagline[1].split("(")[0].trim() : "";
if (taglineText.length === 0 || taglineText.length > 60) {
  failures.push(`docs/launch/product-hunt/listing.md: tagline missing or longer than 60 characters (${taglineText.length})`);
}

if (failures.length > 0) {
  console.error(`Positioning check failed (${failures.length}):`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log(`Positioning check passed (canonical block quoted verbatim in README.md and first-comment.md; one-liner ${oneLiner.length} characters; tagline ${taglineText.length} characters).`);
