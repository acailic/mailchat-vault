#!/usr/bin/env node
// Link check for every tracked Markdown file: resolves relative links and
// image references and fails when a target file (or an in-page anchor of a
// Markdown target) does not exist. Plain Node, no dependencies.
//
// Usage: node scripts/check-links.mjs
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const repoRoot = path.resolve(new URL("..", import.meta.url).pathname);

function trackedMarkdownFiles() {
  const out = execFileSync("git", ["ls-files", "*.md"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  return out.split("\n").filter((line) => line.length > 0);
}

// GitHub-style heading slug: lowercase, drop non-word/space/hyphen chars,
// spaces to hyphens, with -1/-2 disambiguation for repeated headings.
function slugifyHeadings(text) {
  const seen = new Map();
  const slugs = new Set();
  for (const line of text.split("\n")) {
    const heading = line.match(/^\s{0,3}#{1,6}\s+(.*)$/);
    if (!heading) continue;
    const title = heading[1]
      .replace(/`([^`]*)`/g, "$1")
      .replace(/\*\*?([^*]*)\*\*?/g, "$1")
      .replace(/#+\s*$/, "")
      .trim();
    let slug = title
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s_-]/gu, "")
      .replace(/\s/g, "-");
    const count = seen.get(slug) ?? 0;
    seen.set(slug, count + 1);
    if (count > 0) slug = `${slug}-${count}`;
    slugs.add(slug);
  }
  return slugs;
}

function stripCodeBlocks(text) {
  return text.replace(/<!--[\s\S]*?-->/g, "").replace(/```[\s\S]*?```/g, "");
}

const anchorCache = new Map();
function anchorsOf(file) {
  const abs = path.join(repoRoot, file);
  if (!anchorCache.has(abs)) {
    anchorCache.set(
      abs,
      fs.existsSync(abs) ? slugifyHeadings(fs.readFileSync(abs, "utf8")) : new Set(),
    );
  }
  return anchorCache.get(abs);
}

const linkRe = /(!?)\[[^\]]*\]\(\s*<?([^)\s>]+)>?[^)]*\)/g;

let checked = 0;
const broken = [];

for (const file of trackedMarkdownFiles()) {
  const text = stripCodeBlocks(fs.readFileSync(path.join(repoRoot, file), "utf8"));
  for (const match of text.matchAll(linkRe)) {
    const target = match[2];
    checked += 1;
    if (/^(https?:|mailto:|irc:|data:)/.test(target)) continue;
    if (target.startsWith("#")) {
      if (!anchorsOf(file).has(target.slice(1))) {
        broken.push(`${file}: anchor #${target.slice(1)} not found`);
      }
      continue;
    }
    const [pathPart, anchor] = target.split("#");
    const resolved = path.resolve(path.dirname(path.join(repoRoot, file)), pathPart);
    if (!fs.existsSync(resolved)) {
      broken.push(`${file}: target ${target} not found`);
      continue;
    }
    if (anchor && pathPart.endsWith(".md")) {
      const rel = path.relative(repoRoot, resolved);
      if (!anchorsOf(rel).has(anchor)) {
        broken.push(`${file}: anchor ${target} not found`);
      }
    }
  }
}

for (const failure of broken) console.error(`broken: ${failure}`);
console.log(`checked ${checked} links in ${trackedMarkdownFiles().length} tracked .md files`);
if (broken.length > 0) {
  console.error(`${broken.length} broken link(s)`);
  process.exit(1);
}
console.log("all links resolve");
