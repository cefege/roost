import { afterEach, describe, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  TerminalResyncCommandSchema,
  TerminalViewStatus,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS,
  type TerminalScreenSocketSink,
} from "../src/connect/terminal-screen-hub.ts";
import {
  SESSION,
  VIEW_A,
  WORKER,
  disposeHubs,
  makeHarness as makeViewHarness,
  register,
  settle,
  statesFor,
  viewCommand,
} from "./terminal-view-hub-harness.ts";
import {
  EPOCH,
  chunks,
  deltaFrame,
  fullFrame,
} from "./terminal-screen-hub-harness.ts";
import {
  decodedFrames,
  flushMicrotasks,
  makeHarness as makeSchedulerHarness,
  type SchedulerHarness,
} from "./sync-ws-v2-scheduler-harness.ts";

afterEach(disposeHubs);

function resyncCommand(streamId: string, gridEpoch = "", seq = 0n) {
  return create(TerminalResyncCommandSchema, {
    viewId: VIEW_A,
    sessionId: SESSION,
    streamId,
    gridEpoch,
    seq,
  });
}

function schedulerSink(harness: SchedulerHarness): TerminalScreenSocketSink {
  return {
    beginTerminalStream: (sessionId, streamId) =>
      harness.scheduler.beginTerminalStream(harness.ws, sessionId, streamId),
    enqueueTerminalState: (frame, sessionId) =>
      harness.scheduler.enqueueTerminalState(harness.ws, frame, sessionId),
    replaceTerminalSnapshot: (sessionId, streamId, source) =>
      harness.scheduler.replaceTerminalSnapshot(harness.ws, sessionId, streamId, source),
    enqueueTerminalDelta: (sessionId, streamId, frame) =>
      harness.scheduler.enqueueTerminalDelta(harness.ws, sessionId, streamId, frame)
        ? "queued"
        : "needs_snapshot",
    dropTerminalSession: (sessionId) =>
      harness.scheduler.dropTerminalSession(harness.ws, sessionId),
  };
}

function cellFrames(frames: readonly FirehoseFrame[]) {
  return frames.flatMap((frame) => frame.frame.case === "cellGrid" ? [frame.frame.value] : []);
}

describe("terminal view coordinator repair", () => {
  test("same-revision heartbeat recreates a missing scheduler lane without minting a stream", async () => {
    const scheduler = makeSchedulerHarness("viewer-a", true);
    scheduler.socket.data.v2!.announcedSessions.add(SESSION);
    const { hub, sent } = makeViewHarness();
    hub.registerSocket({
      socketId: "socket-a",
      viewerKey: "viewer-a",
      callerFingerprint: "fingerprint-a",
      allowsSession: (sessionId) => sessionId === SESSION,
      sink: schedulerSink(scheduler),
    });
    const command = viewCommand(VIEW_A, 1n);
    hub.handleViewCommand("socket-a", command);
    await settle();

    const streamId = hub.snapshot(SESSION)!.streamId;
    hub.screen.publishFrame(SESSION, fullFrame({
      streamId,
      cols: 80,
      rows: 24,
    }));
    await settle();
    expect(sent).toHaveLength(1);

    scheduler.socket.data.v2!.terminalSessions.delete(SESSION);
    scheduler.socket.sent.length = 0;
    hub.handleViewCommand("socket-a", command);
    hub.screen.publishFrame(SESSION, deltaFrame({
      streamId,
      cols: 80,
      rows: 24,
      baseSeq: 1n,
      seq: 2n,
    }));
    await settle();
    await flushMicrotasks();

    const repaired = cellFrames(decodedFrames(scheduler.socket));
    expect(repaired.map((frame) => ({ full: frame.full, seq: frame.seq }))).toEqual([
      { full: true, seq: 1n },
      { full: false, seq: 2n },
    ]);
    expect(hub.snapshot(SESSION)?.streamId).toBe(streamId);
    expect(sent).toHaveLength(1);

    scheduler.socket.sent.length = 0;
    hub.handleViewCommand("socket-a", command);
    await settle();
    await flushMicrotasks();
    expect(cellFrames(decodedFrames(scheduler.socket))).toEqual([]);
    expect(sent).toHaveLength(1);
  });

  test("terminalResync only seeds cache that proves browser progress", async () => {
    const { hub, snapshotRequests } = makeViewHarness();
    const sink = register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    const streamId = hub.snapshot(SESSION)!.streamId;
    hub.screen.publishFrame(SESSION, fullFrame({
      streamId,
      cols: 80,
      rows: 24,
      seq: 2n,
    }));
    sink.snapshots.length = 0;

    const exact = resyncCommand(streamId, EPOCH, 2n);
    hub.handleResync("socket-a", exact);
    hub.handleResync("socket-a", exact);
    await settle();
    expect(snapshotRequests).toHaveLength(1);
    expect(sink.snapshots).toEqual([]);
    expect(hub.screen.snapshot(SESSION)).toMatchObject({ seq: 2, valid: true });

    hub.screen.publishFrame(SESSION, fullFrame({
      streamId,
      cols: 80,
      rows: 24,
      seq: 3n,
    }));
    expect(sink.snapshots).toHaveLength(1);
    expect(hub.screen.snapshot(SESSION)).toMatchObject({ seq: 3, valid: true });

    snapshotRequests.length = 0;
    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId));
    await settle();
    expect(sink.snapshots).toHaveLength(1);
    expect(snapshotRequests).toEqual([]);

    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 2n));
    await settle();
    expect(sink.snapshots).toHaveLength(1);
    expect(snapshotRequests).toEqual([]);

    sink.snapshots.length = 0;
    expect(hub.screen.resyncSocket("socket-a", SESSION, null)).toBe(true);
    expect(sink.snapshots).toHaveLength(1);
    expect(snapshotRequests).toEqual([]);

    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId, "conflicting-epoch", 2n));
    await settle();
    expect(sink.snapshots).toEqual([]);
    expect(snapshotRequests).toHaveLength(1);
    hub.screen.publishFrame(SESSION, fullFrame({
      streamId,
      cols: 80,
      rows: 24,
      seq: 4n,
    }));

    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 5n));
    await settle();
    expect(sink.snapshots).toEqual([]);
    expect(snapshotRequests).toHaveLength(2);
    hub.screen.publishFrame(SESSION, fullFrame({
      streamId,
      cols: 80,
      rows: 24,
      seq: 5n,
    }));

    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId, "", 2n));
    await settle();
    expect(sink.snapshots).toEqual([]);
    expect(snapshotRequests).toHaveLength(3);
    expect(hub.screen.snapshot(SESSION)).toMatchObject({ seq: 5, valid: true });
  });

  test("retries transient snapshot lookup and send failures without replacing the stream", async () => {
    vi.useFakeTimers();
    let failure: "lookup" | "refuse" | "throw" | null = null;
    try {
      const { hub, sent, snapshotRequests } = makeViewHarness({
        resolveRoute: async () => {
          if (failure === "lookup") {
            failure = null;
            throw new Error("transient snapshot route lookup");
          }
          return { workerFp: WORKER, channel: 7 };
        },
        sendSnapshot: () => {
          const nextFailure = failure;
          failure = null;
          if (nextFailure === "throw") throw new Error("transient snapshot send");
          return nextFailure !== "refuse";
        },
      });
      const sink = register(hub);
      hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
      await settle();
      const streamId = hub.snapshot(SESSION)!.streamId;
      const controlCount = sent.length;
      hub.screen.publishFrame(SESSION, fullFrame({
        streamId,
        cols: 80,
        rows: 24,
        seq: 1n,
      }));

      failure = "lookup";
      hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 1n));
      await settle();
      expect(snapshotRequests).toEqual([]);
      expect(hub.screen.snapshot(SESSION)).toMatchObject({ seq: 1, valid: true });
      vi.advanceTimersByTime(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
      await settle();
      expect(snapshotRequests).toHaveLength(1);
      hub.screen.publishFrame(SESSION, fullFrame({
        streamId,
        cols: 80,
        rows: 24,
        seq: 2n,
      }));

      failure = "refuse";
      hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 2n));
      await settle();
      expect(snapshotRequests).toHaveLength(2);
      vi.advanceTimersByTime(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
      await settle();
      expect(snapshotRequests).toHaveLength(3);
      hub.screen.publishFrame(SESSION, fullFrame({
        streamId,
        cols: 80,
        rows: 24,
        seq: 3n,
      }));

      failure = "throw";
      hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 3n));
      await settle();
      expect(snapshotRequests).toHaveLength(4);
      vi.advanceTimersByTime(TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
      await settle();
      expect(snapshotRequests).toHaveLength(5);
      hub.screen.publishFrame(SESSION, fullFrame({
        streamId,
        cols: 80,
        rows: 24,
        seq: 4n,
      }));

      expect(sent).toHaveLength(controlCount);
      expect(hub.screen.snapshot(SESSION)).toMatchObject({
        streamId,
        seq: 4,
        valid: true,
      });
      expect(hub.snapshot(SESSION)?.unavailable).toBe(false);
      expect(sink.snapshots.length).toBeGreaterThan(0);
    } finally {
      disposeHubs();
      vi.useRealTimers();
    }
  });

  test("waits for route reconciliation after a missing snapshot route", async () => {
    let routeAvailable = true;
    const { hub, sent, snapshotRequests } = makeViewHarness({
      resolveRoute: async () => routeAvailable ? { workerFp: WORKER, channel: 7 } : null,
    });
    const sink = register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    const streamId = hub.snapshot(SESSION)!.streamId;
    hub.screen.publishFrame(SESSION, fullFrame({ streamId, cols: 80, rows: 24 }));

    routeAvailable = false;
    hub.handleResync("socket-a", resyncCommand(streamId, EPOCH, 1n));
    await settle();
    expect(snapshotRequests).toEqual([]);
    expect(hub.screen.snapshot(SESSION)).toMatchObject({ streamId, valid: true });
    expect(hub.snapshot(SESSION)).toMatchObject({ streamId, unavailable: true });
    expect(statesFor(sink, VIEW_A).at(-1)).toMatchObject({
      status: TerminalViewStatus.UNAVAILABLE,
      reason: "snapshot request has no worker route",
    });

    routeAvailable = true;
    hub.routeReconciled(WORKER, [SESSION]);
    await settle();
    expect(sent).toHaveLength(2);
    expect(sent[1]!.streamId).not.toBe(streamId);
    expect(hub.snapshot(SESSION)?.unavailable).toBe(false);
  });
  test("terminalResync does not duplicate a bounded latched replacement request", async () => {
    const { hub, snapshotRequests } = makeViewHarness();
    const sink = register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    const streamId = hub.snapshot(SESSION)!.streamId;
    hub.screen.publishFrame(SESSION, fullFrame({ streamId, cols: 80, rows: 24 }));
    sink.snapshots.length = 0;

    hub.screen.publishFrame(SESSION, fullFrame({ streamId, cols: 79, rows: 24 }));
    await settle();
    expect(snapshotRequests).toHaveLength(1);
    hub.handleResync("socket-a", resyncCommand(streamId));
    await settle();

    expect(snapshotRequests).toHaveLength(1);
    expect(sink.snapshots).toEqual([]);
  });

  test("terminalResync deduplicates invalid-cache requests and skips active assembly", async () => {
    const retry = makeViewHarness();
    register(retry.hub);
    retry.hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    const retryStream = retry.hub.snapshot(SESSION)!.streamId;

    retry.hub.handleResync("socket-a", resyncCommand(retryStream));
    await settle();
    retry.hub.handleResync("socket-a", resyncCommand(retryStream));
    await settle();
    expect(retry.snapshotRequests.map(({ streamId }) => streamId)).toEqual([
      retryStream,
    ]);

    const assembling = makeViewHarness();
    const sink = register(assembling.hub, "assembling-socket");
    assembling.hub.handleViewCommand("assembling-socket", viewCommand(VIEW_A, 1n));
    await settle();
    const assemblingStream = assembling.hub.snapshot(SESSION)!.streamId;
    const source = fullFrame({ streamId: assemblingStream, cols: 80, rows: 24 });
    const midpoint = source.viewportRows.length / 2;
    const parts = chunks(source, [
      source.viewportRows.slice(0, midpoint),
      source.viewportRows.slice(midpoint),
    ]);
    assembling.hub.screen.publishChunk(SESSION, parts[0]!);

    assembling.hub.handleResync("assembling-socket", resyncCommand(assemblingStream, EPOCH, 1n));
    assembling.hub.handleResync("assembling-socket", resyncCommand(assemblingStream, EPOCH, 1n));
    await settle();
    expect(assembling.snapshotRequests).toEqual([]);

    assembling.hub.screen.publishChunk(SESSION, parts[1]!);
    expect(assembling.hub.screen.snapshot(SESSION)).toMatchObject({
      streamId: assemblingStream,
      valid: true,
    });
    expect(sink.snapshots).toHaveLength(1);
    expect(assembling.snapshotRequests).toEqual([]);
  });
});
