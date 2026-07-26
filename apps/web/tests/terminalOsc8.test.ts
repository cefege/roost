import { afterEach, describe, expect, test } from "bun:test";
import {
  osc8TrackerFor,
  processOsc8Chunk,
  pruneOsc8Tracker,
} from "../src/lib/terminalOsc8.ts";

const encoder = new TextEncoder();
const sessionIds = ["osc8-a", "osc8-b"];
const bytes = (text: string) => encoder.encode(text);
const link = (uri: string, text: string) => `\x1b]8;;${uri}\x07${text}\x1b]8;;\x07`;

afterEach(() => {
  for (const sessionId of sessionIds) pruneOsc8Tracker(sessionId);
});

describe("session OSC 8 tracker registry", () => {
  test("retains links received before a pane obtains its tracker", () => {
    processOsc8Chunk("osc8-a", bytes(link("https://example.test/a", "before-visit")));

    expect(osc8TrackerFor("osc8-a").lookup("before-visit")).toBe("https://example.test/a");
  });

  test("preserves a split OSC 8 sequence across byte chunks", () => {
    processOsc8Chunk("osc8-a", bytes("\x1b]8;;https://example.test/split\x07split-"));
    processOsc8Chunk("osc8-a", bytes("label\x1b]8;;\x07"));

    expect(osc8TrackerFor("osc8-a").lookup("split-label")).toBe("https://example.test/split");
  });

  test("isolates mappings by session ID", () => {
    processOsc8Chunk("osc8-a", bytes(link("https://example.test/a", "same-label")));
    processOsc8Chunk("osc8-b", bytes(link("https://example.test/b", "same-label")));

    expect(osc8TrackerFor("osc8-a").lookup("same-label")).toBe("https://example.test/a");
    expect(osc8TrackerFor("osc8-b").lookup("same-label")).toBe("https://example.test/b");
  });

  test("pruning resets one session without affecting another", () => {
    processOsc8Chunk("osc8-a", bytes(link("https://example.test/a", "a-link")));
    processOsc8Chunk("osc8-b", bytes(link("https://example.test/b", "b-link")));

    pruneOsc8Tracker("osc8-a");

    expect(osc8TrackerFor("osc8-a").lookup("a-link")).toBeUndefined();
    expect(osc8TrackerFor("osc8-b").lookup("b-link")).toBe("https://example.test/b");
  });
});
