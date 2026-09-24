// Terminal metadata parser coverage for worker/coordinator compatibility.
// These cases pin the OSC boundary, UTF-8, normalization, and bounded-title
// behavior before either transport side retains or publishes semantic state.

import { describe, expect, test } from "bun:test";
import {
  TERMINAL_METADATA_CAPABILITY,
  TERMINAL_TITLE_MAX_LENGTH,
  TerminalTitleParser,
  normalizeTerminalTitle,
} from "../src/terminal-metadata.ts";

const encoder = new TextEncoder();

describe("terminal metadata title parser", () => {
  test("advertises the fixed rolling-upgrade capability token", () => {
    expect(TERMINAL_METADATA_CAPABILITY).toBe("terminal_metadata_v1");
  });

  test("bridges an ESC/OSC boundary and BEL termination", () => {
    const parser = new TerminalTitleParser();
    expect(parser.push(encoder.encode("\x1b"))).toBeNull();
    expect(parser.push(encoder.encode("]0;build\x07"))).toEqual({
      title: "build",
      dedupKey: "build",
    });
  });

  test("bridges split UTF-8 and ST termination", () => {
    const parser = new TerminalTitleParser();
    const bytes = encoder.encode("\x1b]2;π ⠋ build\x1b\\");
    expect(parser.push(bytes.subarray(0, 7))).toBeNull();
    expect(parser.push(bytes.subarray(7))).toEqual({
      title: "π ⠋ build",
      dedupKey: "π ⠋ build".replace(/⠋/g, "\u2800"),
    });
  });

  test("keeps the latest completed title and strips terminal controls", () => {
    const parser = new TerminalTitleParser();
    expect(parser.push(encoder.encode(
      "\x1b]0;first\x07ignored\x1b]2;last\u0001\u007f\x07",
    ))).toEqual({ title: "last", dedupKey: "last" });
  });

  test("normalizes spinners for dedup while preserving the displayed title", () => {
    expect(normalizeTerminalTitle("running ⠁")).toEqual({
      title: "running ⠁",
      dedupKey: "running \u2800",
    });
  });

  test("caps titles without retaining the oversized source", () => {
    const title = "x".repeat(TERMINAL_TITLE_MAX_LENGTH + 20);
    const parser = new TerminalTitleParser();
    const observation = parser.push(encoder.encode(`\x1b]0;${title}\x07`));
    expect(observation?.title).toBe("x".repeat(TERMINAL_TITLE_MAX_LENGTH));
  });
});
