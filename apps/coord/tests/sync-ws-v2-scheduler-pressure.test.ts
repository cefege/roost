// Covers pressure boundaries that remain at the generic Sync-v2 queue layer.
// Scoped terminal lane recovery lives in sync-ws-v2-ready-ring.test.ts.
// Nonterminal overflow still resets its domain; terminal cell pressure does not.

import { expect, test } from "bun:test";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import {
  V2_NONTERMINAL_MAX_RETAINED_FRAMES,
  V2_TERMINAL_MAX_RETAINED_FRAMES,
} from "../src/connect/sync-ws-v2-state.ts";
import {
  SESSION_A,
  TARGET_SESSION,
  decodedFrames,
  makeCell,
  makeHarness,
  makeState,
} from "./sync-ws-v2-scheduler-harness.ts";

test("terminal generic queue pressure leaves its generation and retained frames live", () => {
  const harness = makeHarness("scheduler-test:terminal-pressure", false);
  const generation = harness.terminal.generation;
  for (let sequence = 1; sequence <= V2_TERMINAL_MAX_RETAINED_FRAMES; sequence++) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(TARGET_SESSION, sequence, false),
      { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
    )).toBe(true);
  }

  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeCell(TARGET_SESSION, V2_TERMINAL_MAX_RETAINED_FRAMES + 1, false),
    { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
  )).toBe(false);
  expect(harness.terminal.generation).toBe(generation);
  expect(harness.terminal.queue).toHaveLength(V2_TERMINAL_MAX_RETAINED_FRAMES);
  expect(decodedFrames(harness.socket).some((frame) => frame.frame.case === "domainReset")).toBe(false);
});

test("terminal partition remains available when nonterminal retention is full", () => {
  const harness = makeHarness("scheduler-test:terminal-partition", false);
  const v2 = harness.socket.data.v2!;
  const workers = v2.domains.get(SyncDomain.WORKERS)!;
  const workspaces = v2.domains.get(SyncDomain.WORKSPACES)!;
  const generation = harness.terminal.generation;
  const filler = makeState(TARGET_SESSION, "partition-filler");
  for (let index = 0; index < V2_NONTERMINAL_MAX_RETAINED_FRAMES / 2; index++) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      filler,
      { domain: SyncDomain.WORKERS, lane: "retained" },
    )).toBe(true);
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      filler,
      { domain: SyncDomain.WORKSPACES, lane: "session" },
    )).toBe(true);
  }

  expect(v2.queuedFrames).toBe(V2_NONTERMINAL_MAX_RETAINED_FRAMES);
  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeCell(TARGET_SESSION, 1, false),
    { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
  )).toBe(true);
  expect(harness.terminal.generation).toBe(generation);
  expect(v2.terminalRetainedFrames).toBe(1);
  expect(workers.queue).toHaveLength(V2_NONTERMINAL_MAX_RETAINED_FRAMES / 2);
  expect(workspaces.queue).toHaveLength(V2_NONTERMINAL_MAX_RETAINED_FRAMES / 2);
});

test("unready terminal cells wait for application admission without a terminal reset", () => {
  const harness = makeHarness("scheduler-test:unready-terminal", false);
  const streamId = "stream-unready-terminal";
  const generation = harness.terminal.generation;
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamId);

  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    SESSION_A,
    streamId,
    makeCell(SESSION_A, 1, false),
  )).toBe(true);
  expect(harness.terminal.generation).toBe(generation);
  expect(harness.terminal.queue).toHaveLength(1);
});
