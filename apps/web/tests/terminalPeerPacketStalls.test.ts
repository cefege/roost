// Per-lane fragment deadlines cannot be extended by traffic on another channel.
// This protects retained control/terminal buffers while history continues flowing.

import { afterEach, expect, test, vi } from "bun:test";
import type { TerminalPeerPacketLane } from "@roost/protocol/terminal-peer";
import { TerminalPeerPacketStalls } from "../src/client/carriers/terminal-peer-packet-stalls.ts";

afterEach(() => vi.useRealTimers());

test("history traffic does not postpone a stalled control fragment", () => {
  vi.useFakeTimers();
  const expired: TerminalPeerPacketLane[] = [];
  let stalls = 0;
  const owner = new TerminalPeerPacketStalls(
    (lane) => { expired.push(lane); return true; },
    () => { stalls += 1; },
  );

  owner.update("control", true);
  vi.advanceTimersByTime(5_000);
  owner.update("history", true);
  vi.advanceTimersByTime(5_000);

  expect(expired).toEqual(["control"]);
  expect(stalls).toBe(1);
  owner.close();
});
