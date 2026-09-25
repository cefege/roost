// Differential corpus for raw-mode PTY streams that rewrite terminal rows fast.
// Pins wterm scrollback, viewport, and cursor margins against xterm.js.
// Consumed by the wterm test gate; production code has no dependency on this file.

import { describe, expect, test } from "bun:test";
import { diffAgainstXterm, type CoreView } from "./xterm-differential.ts";

const COLS = 20;
const ROWS = 6;
const FILL = "1\r\n2\r\n3\r\n4\r\n5\r\n";
const encoder = new TextEncoder();

function caseChunks(...payloads: readonly string[]): Uint8Array[] {
  return [FILL, ...payloads].map((payload) => encoder.encode(payload));
}

function formatView(view: CoreView): string {
  return JSON.stringify({
    scrollbackLength: view.scrollback.length,
    scrollback: view.scrollback,
    viewport: view.viewport,
    cursor: view.cursor,
  });
}

async function expectXtermParity(label: string, payloads: readonly string[]): Promise<void> {
  const result = await diffAgainstXterm(caseChunks(...payloads), COLS, ROWS);
  if (result.firstDivergentChunk !== null) {
    throw new Error([
      `${label}: first divergent chunk ${result.firstDivergentChunk}`,
      `wterm=${formatView(result.wterm)}`,
      `xterm=${formatView(result.xterm)}`,
    ].join("\n"));
  }
  expect(result.firstDivergentChunk).toBeNull();
}

const EXACT_WIDTH_ROW = "B".repeat(COLS);
const REPAINT = `\x1b[2K\x1b[1A\x1b[2K\x1b[G${EXACT_WIDTH_ROW}\n${EXACT_WIDTH_ROW}`;

const STATUS_PREFIX = "✻ ⏺ ⚠️ ✅";
// xterm Unicode 6 and wterm disagree on VS16/✅; CHA fixes the row geometry
// without changing the glyph cells under test.
const STATUS_REWRITE = `\x1b[2K${STATUS_PREFIX}\x1b[${COLS}G `;

describe("wterm matches xterm.js on fast row rewrites", () => {
  test("bare LF after an exact-width row scrolls once", async () => {
    await expectXtermParity("bare LF", ["X".repeat(COLS) + "\nY"]);
  });

  test("200 rapid in-place row rewrites do not duplicate history", async () => {
    await expectXtermParity("rapid repaint", [REPAINT.repeat(200)]);
  });

  test("200 carriage-return row rewrites stay in place", async () => {
    const payload = Array.from({ length: 200 }, () => `\r${EXACT_WIDTH_ROW}`).join("");
    await expectXtermParity("carriage-return repaint", [payload]);
  });

  test("200 cursor-up and CRLF rewrites respect the full screen", async () => {
    const payload = Array.from(
      { length: 200 },
      () => `\x1b[1A\r${EXACT_WIDTH_ROW}\r\n${EXACT_WIDTH_ROW}`,
    ).join("");
    await expectXtermParity("CRLF repaint", [payload]);
  });

  test("CUU clamps to DECSTBM scrolling margins", async () => {
    const payload = `\x1b[2;5r\x1b[3;1H${Array.from({ length: 50 }, () => "\x1b[10AX\n").join("")}`;
    await expectXtermParity("DECSTBM CUU", [payload]);
  });

  test("CUD clamps to DECSTBM scrolling margins", async () => {
    await expectXtermParity("DECSTBM CUD", ["\x1b[2;5r\x1b[3;1H\x1b[10BX"]);
  });

  test("VPR remains clamped to the full screen", async () => {
    await expectXtermParity("VPR screen clamp", ["\x1b[2;5r\x1b[3;1H\x1b[10eX"]);
  });

  test("reverse index cancels a pending exact-width wrap", async () => {
    await expectXtermParity("reverse index", ["X".repeat(COLS) + "\x1bMY"]);
  });

  test("200 exact-width status rewrites stay in place", async () => {
    const payload = Array.from({ length: 200 }, () => `\r${STATUS_REWRITE}`).join("");
    await expectXtermParity("status repaint", [payload]);
  });
});
