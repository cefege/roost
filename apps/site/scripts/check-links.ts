// Link integrity check over the built site: every root-relative href must
// resolve to a real file in dist. Run: bun scripts/check-links.ts
import { readdir } from "node:fs/promises";
import { join, resolve } from "node:path";

const DIST = resolve(import.meta.dir, "..", "dist");

async function htmlFiles(dir: string): Promise<string[]> {
  const out: string[] = [];
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await htmlFiles(full)));
    else if (entry.name.endsWith(".html")) out.push(full);
  }
  return out;
}

const pages = await htmlFiles(DIST);
if (pages.length === 0) {
  console.log(`check-links: no HTML found under ${DIST} (build the site first); nothing to check`);
  process.exit(0);
}

const HREF = /(?:href|src)="([^"]+)"/g;
const failures: string[] = [];
let checked = 0;

for (const page of pages) {
  const html = await Bun.file(page).text();
  const source = page.slice(DIST.length) || "/";
  for (const match of html.matchAll(HREF)) {
    const raw = match[1]!;
    if (!raw.startsWith("/") || raw.startsWith("//")) continue; // external, #frag, mailto:, relative
    const path = raw.split("#")[0]!.split("?")[0]!;
    if (path === "") continue;
    checked += 1;
    const target = resolve(DIST, `.${path}`);
    if (await Bun.file(target).exists()) continue;
    if (await Bun.file(join(target, "index.html")).exists()) continue;
    failures.push(`${source} -> ${raw}`);
  }
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`broken: ${failure}`);
  console.error(`check-links: ${checked} links, ${failures.length} broken`);
  process.exit(1);
}
console.log(`check-links: ${checked} links, 0 broken`);
