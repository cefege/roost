// SGR-1006 mouse reports must not execute as CSI commands.
//
// `CSI < b ; x ; y M/m` is what Roost's browser sends for a mouse event, and it
// shares the `<` private marker the core routes to its Kitty keyboard handler.
// Stock upstream only guards the `u` final, so the press form reaches
// deleteLines and the release form reaches SGR — a raw report replayed into a
// session would silently eat a row or reset colors. The patched core logs and
// ignores them, which is what this pins.

import { describe, expect, test } from "bun:test";
import type { TerminalCore } from "@wterm/core";
import { createWtermCore } from "../src/wterm-core-factory.ts";

const ROWS = ["ROW0", "ROW1", "ROW2", "ROW3"] as const;

function rowText(core: TerminalCore, row: number): string {
  let text = "";
  for (let col = 0; col < core.getCols(); col++) {
    const cell = core.getCell(row, col);
    text += cell.chars ?? String.fromCodePoint(cell.char);
  }
  return text.trimEnd();
}

async function paintedCore(): Promise<TerminalCore> {
  const core = await createWtermCore(40, 6);
  for (const [index, label] of ROWS.entries()) core.writeString(`\x1b[${index + 1};1H${label}`);
  core.writeString("\x1b[2;1H\x1b[1;31m");
  return core;
}

function expectSurfaceUntouched(core: TerminalCore): void {
  expect(ROWS.map((_, row) => rowText(core, row))).toEqual([...ROWS]);
  expect(`${core.getCursor().row},${core.getCursor().col}`).toBe("1,0");
  core.writeString("Z");
  const cell = core.getCell(1, 0);
  expect(cell.fg).toBe(1);
  expect(cell.flags & 0x01).toBe(0x01);
}

describe("mouse reports are inert in the core", () => {
  test("a press report does not delete a row", async () => {
    const core = await paintedCore();
    core.writeString("\x1b[<0;10;5M");
    expectSurfaceUntouched(core);
  });

  test("a release report does not reset colors", async () => {
    const core = await paintedCore();
    core.writeString("\x1b[<0;10;5m");
    expectSurfaceUntouched(core);
  });

  // The guard covers `=` too, and a rebase that dropped that half would
  // otherwise leave every test green.
  test("an `=` marked sequence is inert on the same finals", async () => {
    const core = await paintedCore();
    core.writeString("\x1b[=0;10;5M\x1b[=0;10;5m");
    expectSurfaceUntouched(core);
  });

  test("the kitty keyboard final on the same marker is still routed", async () => {
    const core = await createWtermCore(40, 6);
    core.writeString("\x1b[?u");
    expect(core.getResponse()).toBe("\x1b[?0u");
  });
});
