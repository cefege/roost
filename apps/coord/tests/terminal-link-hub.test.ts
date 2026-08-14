import { describe, expect, test } from "bun:test";
import { SessionEvent } from "@roost/shared/wire";
import {
  globalBytesBus,
  sessionBus,
  terminalLinkBus,
  type BoundedBus,
} from "../src/buses.ts";
import { startTerminalLinkHub } from "../src/terminal-link-hub.ts";

const encoder = new TextEncoder();
const SESSION_A = "00000000-0000-4000-8000-0000000000a1";
const SESSION_B = "00000000-0000-4000-8000-0000000000b2";

type TerminalLinkMapping = Parameters<BoundedBus<{
  session_id: string;
  text: string;
  uri: string;
}>["publish"]>[0];

function feed(sessionId: string, text: string): void {
  globalBytesBus.publish({ session_id: sessionId, bytes: encoder.encode(text) });
}

function collectMappings(): {
  mappings: TerminalLinkMapping[];
  stop: () => void;
} {
  const mappings: TerminalLinkMapping[] = [];
  const unsubscribe = terminalLinkBus.subscribe((mapping) => mappings.push(mapping));
  const stopHub = startTerminalLinkHub();
  return {
    mappings,
    stop: () => {
      unsubscribe();
      stopHub();
    },
  };
}

describe("terminal-link-hub", () => {
  test("publishes only completed compact mappings from BEL and split ST links", () => {
    const harness = collectMappings();
    feed(SESSION_A, "ordinary output\n");
    feed(SESSION_A, "\x1b]8;;https://example.test/bel\x07bel-label");
    expect(harness.mappings).toEqual([]);
    feed(SESSION_A, "\x1b]8;;\x07");
    feed(SESSION_B, "\x1b]8;;https://example.test/st\x1b\\split-");
    feed(SESSION_B, "label\x1b]8;;\x1b");
    expect(harness.mappings).toHaveLength(1);
    feed(SESSION_B, "\\");

    expect(harness.mappings).toEqual([
      {
        session_id: SESSION_A,
        text: "bel-label",
        uri: "https://example.test/bel",
      },
      {
        session_id: SESSION_B,
        text: "split-label",
        uri: "https://example.test/st",
      },
    ]);
    expect(Object.keys(harness.mappings[0]!).sort()).toEqual(["session_id", "text", "uri"]);
    harness.stop();
  });

  test("isolates parser state and prunes an incomplete link on session close", () => {
    const harness = collectMappings();
    feed(SESSION_A, "\x1b]8;;https://example.test/stale\x07stale-label");
    feed(SESSION_B, "\x1b]8;;https://example.test/b\x07b-label\x1b]8;;\x07");
    sessionBus.publish(SessionEvent.parse({
      kind: "closed",
      session_id: SESSION_A,
      exit_code: 0,
      ts: 1_780_000_000_000,
    }));
    feed(SESSION_A, "\x1b]8;;\x07");

    expect(harness.mappings).toEqual([{
      session_id: SESSION_B,
      text: "b-label",
      uri: "https://example.test/b",
    }]);
    harness.stop();
  });

  test("shares one subscription and clears parser state on final disposal", () => {
    const mappings: TerminalLinkMapping[] = [];
    const unsubscribe = terminalLinkBus.subscribe((mapping) => mappings.push(mapping));
    const stopFirst = startTerminalLinkHub();
    const stopSecond = startTerminalLinkHub();
    feed(SESSION_A, "\x1b]8;;https://example.test/once\x07once\x1b]8;;\x07");
    expect(mappings).toHaveLength(1);

    feed(SESSION_A, "\x1b]8;;https://example.test/disposed\x07partial");
    stopFirst();
    stopSecond();
    const stopRestarted = startTerminalLinkHub();
    feed(SESSION_A, "\x1b]8;;\x07");
    expect(mappings).toHaveLength(1);

    stopRestarted();
    unsubscribe();
  });
});
