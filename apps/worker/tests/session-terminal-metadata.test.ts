// Worker semantic terminal metadata state coverage.
// These tests pin bounded retained facts, reconnect replay, and refusal behavior
// without constructing a terminal core or retaining a raw PTY buffer.

import { afterEach, expect, setSystemTime, test, vi } from "bun:test";
import { asWorkerFp } from "@roost/shared/wire";
import { SessionManager } from "../src/session-manager.ts";
import {
  TERMINAL_METADATA_DISPATCH_FRAME_BUDGET,
  flushTerminalMetadata,
  observeTerminalMetadata,
  setTerminalMetadataNegotiated,
} from "../src/session-terminal-metadata.ts";
import type { TerminalMetadataFrame, TransportSendResult } from "../src/transport/coord-link-types.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const managers = new Set<SessionManager>();

function createManager(
  send: (metadata: TerminalMetadataFrame) => TransportSendResult,
  channelIds: readonly number[] = [7],
): SessionManager {
  const manager = new SessionManager({
    workerFp: asWorkerFp("cd".repeat(32)),
    sink: new SessionEventTestSink(),
    sendTerminalMetadataUpstream: send,
  });
  for (const channelId of channelIds) manager.sessions.set(channelId, {} as never);
  managers.add(manager);
  return manager;
}

async function flushMicrotasks(): Promise<void> {
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

afterEach(async () => {
  vi.useRealTimers();
  for (const manager of managers) {
    manager.terminalMetadataByChannel.clear();
    manager.terminalMetadataReadyRing.clear();
    manager.sessions.clear();
  }
  managers.clear();
  setSystemTime();
  await flushMicrotasks();
});

test("sends one semantic title/activity record after capability acknowledgement", async () => {
  const sent: TerminalMetadataFrame[] = [];
  const manager = createManager((metadata) => {
    sent.push({ ...metadata });
    return "sent";
  });

  observeTerminalMetadata(manager, 7, new TextEncoder().encode("\x1b"));
  observeTerminalMetadata(manager, 7, new TextEncoder().encode("]0;π ⠋ build\x07"));
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();

  expect(sent).toHaveLength(1);
  expect(sent[0]).toMatchObject({
    channelId: 7,
    titleChanged: true,
    title: "π ⠋ build",
    activityChanged: true,
  });
  expect(sent[0]!.activityTsMs).toBeGreaterThan(0);
});

test("replays bounded latest metadata after a reconnect capability cutover", async () => {
  const sent: TerminalMetadataFrame[] = [];
  const manager = createManager((metadata) => {
    sent.push({ ...metadata });
    return "sent";
  });

  observeTerminalMetadata(manager, 7, new TextEncoder().encode("\x1b]2;replay me\x1b\\"));
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();
  setTerminalMetadataNegotiated(manager, false);
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();

  expect(sent.map((metadata) => metadata.title)).toEqual(["replay me", "replay me"]);
  expect(manager.terminalMetadataByChannel.get(7)?.title).toBe("replay me");
});

test("throttles steady worker activity publications", async () => {
  const sent: TerminalMetadataFrame[] = [];
  const manager = createManager((metadata) => {
    sent.push({ ...metadata });
    return "sent";
  });

  setSystemTime(new Date(1_000));
  setTerminalMetadataNegotiated(manager, true);
  observeTerminalMetadata(manager, 7, new Uint8Array([0x61]));
  await flushMicrotasks();
  expect(sent).toHaveLength(1);
  expect(sent[0]?.activityTsMs).toBe(1_000);

  setSystemTime(new Date(60_999));
  observeTerminalMetadata(manager, 7, new Uint8Array([0x62]));
  await flushMicrotasks();
  expect(sent).toHaveLength(1);

  setSystemTime(new Date(61_000));
  observeTerminalMetadata(manager, 7, new Uint8Array([0x63]));
  await flushMicrotasks();
  expect(sent).toHaveLength(2);
  expect(sent[1]?.activityTsMs).toBe(61_000);
});

test("replays the latest activity retained during worker throttling", async () => {
  const sent: TerminalMetadataFrame[] = [];
  const manager = createManager((metadata) => {
    sent.push({ ...metadata });
    return "sent";
  });

  setSystemTime(new Date(1_000));
  setTerminalMetadataNegotiated(manager, true);
  observeTerminalMetadata(manager, 7, new Uint8Array([0x61]));
  await flushMicrotasks();

  setSystemTime(new Date(2_000));
  observeTerminalMetadata(manager, 7, new Uint8Array([0x62]));
  await flushMicrotasks();
  expect(sent).toHaveLength(1);

  setTerminalMetadataNegotiated(manager, false);
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();
  expect(sent).toHaveLength(2);
  expect(sent[1]?.activityTsMs).toBe(2_000);
});

test("does not republish activity immediately after a delayed replay", async () => {
  const sent: TerminalMetadataFrame[] = [];
  const manager = createManager((metadata) => {
    sent.push({ ...metadata });
    return "sent";
  });

  vi.useFakeTimers();
  setSystemTime(new Date(1_000));
  setTerminalMetadataNegotiated(manager, true);
  observeTerminalMetadata(manager, 7, new Uint8Array([0x61]));
  await flushMicrotasks();
  expect(sent).toHaveLength(1);

  setTerminalMetadataNegotiated(manager, false);
  setSystemTime(new Date(61_000));
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();
  expect(sent).toHaveLength(2);
  expect(sent[1]?.activityTsMs).toBe(1_000);

  observeTerminalMetadata(manager, 7, new Uint8Array([0x62]));
  await flushMicrotasks();
  expect(sent).toHaveLength(2);
});

test("does not spin after a refused semantic send", async () => {
  let attempts = 0;
  const manager = createManager(() => {
    attempts += 1;
    return "dropped";
  });

  observeTerminalMetadata(manager, 7, new TextEncoder().encode("\x1b]0;blocked\x07"));
  setTerminalMetadataNegotiated(manager, true);
  await flushMicrotasks();
  await Promise.resolve();

  expect(attempts).toBe(1);
  flushTerminalMetadata(manager);
  expect(attempts).toBe(2);
});

test("yields to the event loop after one full metadata batch", async () => {
  vi.useFakeTimers();
  const sent: TerminalMetadataFrame[] = [];
  const channelIds = Array.from(
    { length: TERMINAL_METADATA_DISPATCH_FRAME_BUDGET + 1 },
    (_, index) => index + 1,
  );
  const manager = createManager((metadata) => {
    sent.push(metadata);
    return "sent";
  }, channelIds);

  for (const channelId of channelIds) {
    observeTerminalMetadata(manager, channelId, new Uint8Array([channelId]));
  }
  setTerminalMetadataNegotiated(manager, true);
  flushTerminalMetadata(manager);
  expect(sent).toHaveLength(TERMINAL_METADATA_DISPATCH_FRAME_BUDGET);
  await flushMicrotasks();
  expect(sent).toHaveLength(TERMINAL_METADATA_DISPATCH_FRAME_BUDGET);

  await Promise.resolve();
  expect(sent).toHaveLength(TERMINAL_METADATA_DISPATCH_FRAME_BUDGET);
  vi.advanceTimersByTime(1);
  expect(sent).toHaveLength(TERMINAL_METADATA_DISPATCH_FRAME_BUDGET + 1);
});
