// Regenerate apps/web/src/chat-render-real.json — the fixture the /chat-render-real.html
// harness renders through the REAL OmpChatPane. Parses a live omp transcript with
// the worker's parseOmpLine so the fixture is real ChatMessage[] (not synthetic).
//
//   bun apps/web/scripts/genChatFixture.ts [path/to/session.jsonl] [--full]
//
// No arg → picks the transcript under ~/.omp/agent/sessions/-Code-idea that covers
// the most tool renderers (the fixture's job: prove every <omp-tool-view> card).
// Default trims to a compact subset — the head of the thread plus one call/result
// pair per distinct tool — so HMR stays fast and the repo stays small; --full keeps
// the entire session for heavier manual review.

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
  // Most tool renderers wins; block-kind coverage is the floor, recency the tiebreak.
  const scored = files
    .map((f) => ({ f, msgs: parseFile(f) }))
    .map(({ f, msgs }) => ({
      f,
      kinds: new Set(msgs.flatMap((m) => m.blocks.map((b) => b.kind))).size,
      tools: new Set(msgs.flatMap((m) => m.blocks.filter((b) => b.kind === "toolCall").map((b) => b.name))).size,
    }))
    .filter((s) => s.kinds >= 5)
    .sort((a, b) => b.tools - a.tools || b.f.localeCompare(a.f));
  file = scored[0]?.f;
}
if (!file) { console.error("no session found"); process.exit(1); }

const all = parseFile(file);
let picked = all;
if (!full) {
  const keep = new Set(all.slice(0, 22));
  const covered = new Set<string>();
  for (const m of all) {
    for (const b of m.blocks) {
      if (b.kind !== "toolCall" || covered.has(b.name)) continue;
      covered.add(b.name);
      keep.add(m);
      const res = all.find((r) => r.blocks.some((x) => x.kind === "toolResult" && x.callId === b.callId));
      if (res) keep.add(res);
    }
  }
  // Guarantee prose: a tool-dense head can hold zero user/assistant text, which
  // leaves the transcript chrome (gutter labels, markdown) unproven.
  for (const role of ["user", "assistant", "developer"] as const) {
    let n = 0;
    for (const m of all) {
      if (m.role !== role || !m.blocks.some((b) => b.kind === "text")) continue;
      keep.add(m);
      if (++n >= 3) break;
    }
  }
  const img = all.find((m) => m.role === "toolResult" && m.blocks.some((b) => b.kind === "image"));
  const comp = all.find((m) => m.role === "developer" && m.blocks.some((b) => b.kind === "text" && b.text.includes("compacted")));
  for (const extra of [img, comp]) if (extra) keep.add(extra);
  picked = all.filter((m) => keep.has(m));   // transcript order, not discovery order
}
writeFileSync(OUT, JSON.stringify(picked));
const kinds = [...new Set(picked.flatMap((m) => m.blocks.map((b) => b.kind)))].sort();
console.log(`wrote ${picked.length} messages (${JSON.stringify(kinds)}) → ${OUT}`);
