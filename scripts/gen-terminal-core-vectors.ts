// Generates the terminal-core conformance family.
//
// Two oracles, and which one a vector came from is recorded in the vector
// itself, because they answer different questions.
//
// `xterm` — the byte stream is driven through `@xterm/headless` and the
// expected state is what it produced. xterm is an independent terminal
// implementation, so a vector from it says something a Rust test that asserts
// "whatever the Rust core does" cannot: it says the Rust core agrees with a
// terminal that has nothing to do with it.
//
// `patch` — the expected state is written by hand from
// `scripts/wterm-0.5.0-roost.patch`, and the vector names the hunk. These are
// the behaviours the v2 Zig core was patched to have and xterm does not
// share; an xterm-derived vector would record the wrong answer. The generator
// still drives xterm and prints what it did, so the difference between the
// patched behaviour and the unpatched one is visible rather than asserted.
//
// Run: bun scripts/gen-terminal-core-vectors.ts
// The output is committed; regenerate only when a case is added or its bytes
// change, and diff the result — a vector that moved without its bytes moving
// is a behaviour change and deserves a commit message that says so.

import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Terminal } from "@xterm/headless";

import { CASES, type Case } from "./terminal-core-cases.ts";

const REPO = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = join(REPO, "protocol", "conformance", "terminal-core");

type Oracle =
  /** Derived by driving `@xterm/headless`, an independent implementation. */
  | "xterm"
  /** Hand-written from `scripts/wterm-0.5.0-roost.patch`; the vector names the hunk. */
  | "patch"
  /**
   * The v2 Zig core was patched to do something `alacritty_terminal` does
   * not, and the gap is not yet closed. `expected` records what this core does
   * TODAY and `diverges_from_v2` records what v2 did, so the difference is
   * asserted and greppable instead of silently wrong. When a divergence is
   * closed, this vector fails and the case is promoted to `patch`.
   */
  | "known-divergence";

interface Expected {
  viewport: string[];
  scrollback: string[];
  cursor: { row: number; col: number };
  alt_screen: boolean;
  modes: {
    cursor_keys_app: boolean;
    bracketed_paste: boolean;
    mouse_tracking: 0 | 1000 | 1002;
    mouse_sgr: boolean;
    focus_events: boolean;
  };
}

interface Case {
  name: string;
  /** What the bytes prove. Written into the vector so a reader does not have to
   *  infer it from the diff. */
  about: string;
  cols: number;
  rows: number;
  /** Byte chunks, fed in order. A split matters: a terminal that only
   *  reassembles correctly across chunk boundaries is a different claim. */
  chunks: string[];
  oracle: Oracle;
  /** For `patch` cases, the hunk in the v2 patch this pins. */
  patch?: string;
  /** For `patch` cases, the state the patch produces, written by hand. */
  expect?: Expected;
  /** For `known-divergence` cases, what the v2 core did instead. */
  diverges_from_v2?: string;
  /** A resize after the chunks, which is a separate wire event from a write. */
  resize?: { cols: number; rows: number };
}

interface XtermView {
  viewport: string[];
  scrollback: string[];
  cursor: { row: number; col: number };
  alt_screen: boolean;
  modes: Expected["modes"];
}

async function driveWithXterm(entry: Case): Promise<XtermView> {
  const terminal = new Terminal({
    cols: entry.cols,
    rows: entry.rows,
    allowProposedApi: true,
    scrollback: 10_000,
  });
  for (const chunk of entry.chunks) {
    // `write` parses on a later tick, so the callback is the only point at
    // which the buffer reflects the bytes.
    await new Promise<void>((resolve) => terminal.write(chunk, resolve));
  }
  if (entry.resize) terminal.resize(entry.resize.cols, entry.resize.rows);

  const buffer = terminal.buffer.active;
  // A line past the end of the buffer is a blank line, not a crash: a resize
  // can leave the buffer shorter than the grid it is asked to describe, and the
  // vector for that case is hand-written from the patch anyway.
  const rowText = (row: number): string =>
    buffer.getLine(row)?.translateToString(true).trimEnd() ?? "";
  const scrollback: string[] = [];
  for (let row = 0; row < buffer.baseY; row++) scrollback.push(rowText(row));
  const viewport: string[] = [];
  const end = Math.min(buffer.baseY + entry.rows, buffer.length);
  for (let row = buffer.baseY; row < end; row++) viewport.push(rowText(row));
  // xterm names mouse tracking; the wire names the mode. Anything-motion is
  // folded away by the core, exactly as the v2 core folded it.
  const tracking = terminal.modes.mouseTracking;
  const mouseTracking = tracking === "button" ? 1000 : tracking === "any" ? 1000 : 0;
  return {
    viewport,
    scrollback,
    cursor: { row: buffer.cursorY, col: buffer.cursorX },
    alt_screen: buffer.type === "alternate",
    modes: {
      // Coerced: xterm reports an unset mode as `undefined`, which
      // `JSON.stringify` drops, and a vector missing a key is a vector the
      // runner cannot read.
      cursor_keys_app: terminal.modes.applicationCursorKeysMode === true,
      bracketed_paste: terminal.modes.bracketedPasteMode === true,
      mouse_tracking: mouseTracking as 0 | 1000,
      mouse_sgr: terminal.modes.mouseSgrEncoding === true,
      focus_events: terminal.modes.focusEvent === true,
    },
  };
}

function encodeChunks(entry: Case): string[] {
  return entry.chunks.map((chunk) => Buffer.from(chunk, "utf8").toString("base64"));
}

async function main(): Promise<void> {
  mkdirSync(OUT_DIR, { recursive: true });
  for (const stale of readdirSync(OUT_DIR)) {
    if (stale.endsWith(".json")) rmSync(join(OUT_DIR, stale));
  }
  const written: string[] = [];
  for (const entry of CASES) {
    const xterm = await driveWithXterm(entry);
    const expected: Expected =
      entry.oracle === "xterm" ? xterm : (entry.expect as Expected);
    if (entry.oracle === "known-divergence") {
      console.log(`  ${entry.name}: KNOWN DIVERGENCE — ${entry.diverges_from_v2}`);
    }
    if (entry.oracle === "patch") {
      const same =
        JSON.stringify(xterm.viewport) === JSON.stringify(expected.viewport) &&
        xterm.cursor.row === expected.cursor.row &&
        xterm.cursor.col === expected.cursor.col;
      console.log(
        `  ${entry.name}: patched=${same ? "SAME as xterm" : "DIFFERS from xterm"} ` +
          `(xterm cursor ${xterm.cursor.row},${xterm.cursor.col}; ` +
          `patched cursor ${expected.cursor.row},${expected.cursor.col})`,
      );
    }
    const vector = {
      name: entry.name,
      about: entry.about,
      oracle: entry.oracle,
      ...(entry.patch ? { patch: entry.patch } : {}),
      ...(entry.diverges_from_v2 ? { diverges_from_v2: entry.diverges_from_v2 } : {}),
      ...(entry.blocked_on ? { blocked_on: entry.blocked_on } : {}),
      cols: entry.cols,
      rows: entry.rows,
      ...(entry.resize ? { resize: entry.resize } : {}),
      chunks: encodeChunks(entry),
      expected,
    };
    writeFileSync(join(OUT_DIR, `${entry.name}.json`), `${JSON.stringify(vector, null, 2)}\n`);
    written.push(entry.name);
  }
  console.log(`\nwrote ${written.length} vectors to protocol/conformance/terminal-core/`);

  // A vector whose bytes round-trip must be byte-identical after a write, or
  // the Rust runner is being handed something the generator did not intend.
  for (const name of written) {
    const vector = JSON.parse(readFileSync(join(OUT_DIR, `${name}.json`), "utf8"));
    for (const [index, encoded] of (vector.chunks as string[]).entries()) {
      const decoded = Buffer.from(encoded, "base64").toString("utf8");
      if (Buffer.from(decoded, "utf8").toString("base64") !== encoded) {
        throw new Error(`${name} chunk ${index} does not survive base64`);
      }
    }
  }
}

await main();
