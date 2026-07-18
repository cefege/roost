// Drives the ported claude manifest (detect/claude-manifest.ts) through the
// engine (detect/manifest-engine.ts) with synthetic claude screens, asserting
// the screen verdict → AgentStatus mapping. These are the states the
// --settings hooks miss (everything blocked-but-not-a-permission-request).

import { test, expect } from "bun:test";
import { evaluate } from "../src/detect/manifest-engine.ts";
import { CLAUDE_RULES } from "../src/detect/claude-manifest.ts";
import { screenStatus, readScreenText } from "../src/detect/screen-detect.ts";

const RULE = "─────────────────────────";

function status(screen: string, oscTitle = ""): string | null {
  return screenStatus(evaluate(CLAUDE_RULES, { screen, oscTitle, oscProgress: "" }));
}

test("bash permission prompt → needs-input", () => {
  const screen = [
    "⏺ I'll run a command", "", RULE, " Bash command", " ls -la", RULE,
    " Do you want to proceed?", " ❯ 1. Yes", "   2. No", "", " esc to cancel",
  ].join("\n");
  expect(status(screen)).toBe("needs-input");
});

test("live blocked selection form → needs-input", () => {
  const screen = [
    "Pick an option", "", RULE,
    " Choose a model", " enter to select · esc to cancel", " ↑/↓ to navigate",
  ].join("\n");
  expect(status(screen)).toBe("needs-input");
});

test("live prompt box (❯, no selection) → idle", () => {
  const screen = ["⏺ Done.", "", RULE, " ❯ type your message", RULE, "  ? for shortcuts"].join("\n");
  expect(status(screen)).toBe("idle");
});

test("transcript viewer → no opinion (freeze)", () => {
  const screen = [" Showing detailed transcript", "", " ctrl+o to toggle"].join("\n");
  expect(status(screen)).toBeNull();
});

test("model picker menu → no opinion (freeze)", () => {
  const screen = [" Select model", " enter to set as default", " esc to cancel"].join("\n");
  expect(status(screen)).toBeNull();
});

test("working spinner in window title → running", () => {
  // ⠹ = U+2839, inside the braille spinner range claude animates in the title.
  expect(status("⏺ idle prompt box body", "⠹ Building the thing")).toBe("running");
});

test("plain output, no pattern → no opinion", () => {
  expect(status(["some build log", "$ ls", "file.txt"].join("\n"))).toBeNull();
});

test("readScreenText trims trailing blanks + maps codepoints", () => {
  // Minimal TerminalCore stand-in: a 5x2 grid spelling "hi" on row 0.
  const grid = [[104, 105, 0, 0, 0], [0, 0, 0, 0, 0]];
  const fake = {
    getCols: () => 5,
    getRows: () => 2,
    getCell: (r: number, c: number) => ({ char: grid[r][c] }),
    getTitle: () => null,
  } as unknown as Parameters<typeof readScreenText>[0];
  expect(readScreenText(fake)).toBe("hi\n");
});
