// Terminal ⇄ web parity, mechanically, over the real local corpus.
//
// `tuiRows(lines)` is a port of omp 17.1.3's ChatTranscriptBuilder — the rows
// the TERMINAL paints. `roostRows(parsed)` is the same projection over what
// parse.ts hands the web pane. Parity IS the two being deep-equal, row for row,
// in order. A row the terminal paints that the pane drops shows up here first.
//
// This is a LOCAL truth check, not a CI gate: without ~/.omp/agent/sessions the
// whole test skips with a logged reason rather than flaking. Capped at the 200
// newest transcripts so a growing corpus cannot turn it into a slow test.

import { test, expect } from "bun:test";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import type { ChatMessage } from "@roost/shared/chat/wire";
import { parseOmpLine } from "../../../src/chat/omp/parse.ts";
import { roostRows, tuiRows, type TuiRow } from "../../../src/chat/omp/tui-rows.ts";

const SESSIONS = join(process.env.HOME ?? "", ".omp/agent/sessions");
const MAX_TRANSCRIPTS = 200;

/** Stable, readable identity of a row — the unit both projections are compared
 *  on. Deliberately not JSON.stringify: key order must not decide parity, and a
 *  failure message has to be legible at a glance. */
function rowKey(row: TuiRow): string {
  switch (row.kind) {
    case "user": return `user(synthetic=${row.synthetic})`;
    case "assistant": return "assistant";
    case "tool": return `tool(${row.name}#${row.callId})`;
    case "notice": return `notice(${row.level})`;
    case "summary": return `summary(${row.variant})`;
    case "custom": return `custom(${row.customType})`;
    case "exec": return `exec(${row.lang})`;
    case "fileMention": return `fileMention(${row.count})`;
  }
}

/** ~/.omp/agent/sessions/<project>/<transcript>.jsonl, newest first. */
function corpus(): string[] {
  const paths: string[] = [];
  for (const project of readdirSync(SESSIONS, { withFileTypes: true })) {
    if (!project.isDirectory()) continue;
    const dir = join(SESSIONS, project.name);
    for (const name of readdirSync(dir)) {
      if (name.endsWith(".jsonl")) paths.push(join(dir, name));
    }
  }
  return paths
    .map((path) => ({ path, mtime: statSync(path).mtimeMs }))
    .sort((a, b) => b.mtime - a.mtime)
    .slice(0, MAX_TRANSCRIPTS)
    .map((f) => f.path);
}

if (!existsSync(SESSIONS)) console.log(`tui-parity: skipped — no omp corpus at ${SESSIONS}`);

test.skipIf(!existsSync(SESSIONS))("live corpus: every row omp's TUI paints reaches the pane", () => {
  const files = corpus();
  expect(files.length).toBeGreaterThan(0);

  const mismatches: string[] = [];
  let rows = 0;
  for (const path of files) {
    const lines = readFileSync(path, "utf8").split("\n");
    const tui = tuiRows(lines).map(rowKey);
    const parsed = lines.map((l) => parseOmpLine(l)).filter((m): m is ChatMessage => m !== null);
    const roost = roostRows(parsed).map(rowKey);
    rows += tui.length;
    if (tui.length === roost.length && tui.every((k, i) => k === roost[i])) continue;

    let i = 0;
    while (i < tui.length && i < roost.length && tui[i] === roost[i]) i++;
    mismatches.push(
      `${path}\n    rows: tui=${tui.length} roost=${roost.length}` +
      `\n    first diff at ${i}: tui=${tui[i] ?? "<end>"} roost=${roost[i] ?? "<end>"}` +
      `\n    tui   [${Math.max(0, i - 2)}..]: ${tui.slice(Math.max(0, i - 2), i + 3).join(" | ")}` +
      `\n    roost [${Math.max(0, i - 2)}..]: ${roost.slice(Math.max(0, i - 2), i + 3).join(" | ")}`,
    );
  }

  // A projection that emitted nothing would "match" trivially.
  expect(rows).toBeGreaterThan(0);
  expect(mismatches.join("\n\n")).toBe("");
});
