// Coordinator title hub coverage for semantic worker metadata.
// OSC parsing boundaries live in shared terminal-metadata tests; this suite
// pins retained normalization, spinner deduplication, and Sync publication.

import { describe, expect, it } from "bun:test";
import {
  getTitleSnapshot,
  observeTerminalTitle,
  startTerminalTitleHub,
} from "../src/terminal-title-hub.ts";
import { titleBus } from "../src/buses.ts";

function collect(sessionId: string): { got: string[]; stop: () => void } {
  const got: string[] = [];
  const unsubscribe = titleBus.subscribe((message) => {
    if (message.session_id === sessionId) got.push(message.title);
  });
  const stopHub = startTerminalTitleHub();
  return { got, stop: () => { unsubscribe(); stopHub(); } };
}

describe("terminal-title-hub", () => {
  it("normalizes an incoming semantic title before publication", () => {
    const sessionId = "title-controls";
    const collector = collect(sessionId);
    observeTerminalTitle(sessionId, "line1\ttab\rmid");
    collector.stop();
    expect(collector.got).toEqual(["line1tabmid"]);
  });

  it("caps an oversized semantic title before fan-out", () => {
    const sessionId = "title-huge";
    const collector = collect(sessionId);
    observeTerminalTitle(sessionId, "x".repeat(5_000));
    collector.stop();
    expect(collector.got).toEqual(["x".repeat(256)]);
  });

  it("publishes only meaningful title changes", () => {
    const sessionId = "title-dedupe";
    const collector = collect(sessionId);
    observeTerminalTitle(sessionId, "same");
    observeTerminalTitle(sessionId, "same");
    observeTerminalTitle(sessionId, "different");
    collector.stop();
    expect(collector.got).toEqual(["same", "different"]);
  });

  it("collapses spinner animation while preserving state and label edges", () => {
    const sessionId = "title-spinner";
    const collector = collect(sessionId);
    observeTerminalTitle(sessionId, "π > waiting");
    observeTerminalTitle(sessionId, "π ⠋ waiting");
    observeTerminalTitle(sessionId, "π ⠙ waiting");
    observeTerminalTitle(sessionId, "π ⠙ other task");
    collector.stop();
    expect(collector.got).toEqual([
      "π > waiting",
      "π ⠋ waiting",
      "π ⠙ other task",
    ]);
  });

  it("retains the displayed title rather than its deduplication key", () => {
    const sessionId = "title-snapshot";
    const collector = collect(sessionId);
    observeTerminalTitle(sessionId, "π ⠸ shipping");
    const snapshot = getTitleSnapshot().find((entry) => entry.session_id === sessionId);
    collector.stop();
    expect(snapshot?.title).toBe("π ⠸ shipping");
  });
});
