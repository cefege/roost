// Regenerate apps/web/src/chat-render-real.json — the fixture the /chat-render-real.html
// harness renders through the REAL OmpChatPane. Parses a live omp transcript with
// the worker's parseOmpLine so the fixture is real ChatMessage[] (not synthetic).
//
//   bun apps/web/scripts/genChatFixture.ts [path/to/session.jsonl] [--full]
//
// No arg → picks the newest transcript under ~/.omp/agent/sessions/-Code-idea that
// covers all block kinds. Default trims to a compact subset (fast HMR, small repo
// footprint); --full keeps the entire session for heavier manual review.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { parseOmpLine } from "../../worker/src/chat/omp/parse.ts";
import type { ChatMessage } from "@roost/shared/chat/wire";

const OUT = join(import.meta.dir, "../src/chat-render-real.json");
const SESSIONS = join(process.env.HOME ?? "", ".omp/agent/sessions/-Code-idea");

function parseFile(path: string): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const line of readFileSync(path, "utf8").split("\n")) {
    if (!line) continue;
    const m = parseOmpLine(line);
    if (m) out.push(m);
  }
  return out;
}

const argPath = process.argv.find((a) => a.endsWith(".jsonl"));
const full = process.argv.includes("--full");

let file = argPath;
if (!file) {
  const files = readdirSync(SESSIONS).filter((f) => f.endsWith(".jsonl")).map((f) => join(SESSIONS, f));
  // newest that exercises every block kind
  const scored = files
    .map((f) => ({ f, msgs: parseFile(f) }))
    .filter(({ msgs }) => {
      const kinds = new Set(msgs.flatMap((m) => m.blocks.map((b) => b.kind)));
      return kinds.size >= 5;
    })
    .sort((a, b) => b.f.localeCompare(a.f));
  file = scored[0]?.f;
}
if (!file) { console.error("no session found"); process.exit(1); }

const all = parseFile(file);
let picked = all;
if (!full) {
  const head = all.slice(0, 22);
  const img = all.find((m) => m.role === "toolResult" && m.blocks.some((b) => b.kind === "image"));
  const comp = all.find((m) => m.role === "developer" && m.blocks.some((b) => b.kind === "text" && b.text.includes("compacted")));
  picked = [...head];
  for (const extra of [img, comp]) if (extra && !picked.includes(extra)) picked.push(extra);
}
writeFileSync(OUT, JSON.stringify(picked));
const kinds = [...new Set(picked.flatMap((m) => m.blocks.map((b) => b.kind)))].sort();
console.log(`wrote ${picked.length} messages (${JSON.stringify(kinds)}) → ${OUT}`);
