import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  TerminalResyncCommandSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { TerminalScreenSocketSink } from "../src/connect/terminal-screen-hub.ts";
import {
  DASHBOARD,
  SESSION,
  VIEW_A,
  disposeHubs,
  makeHarness as makeViewHarness,
  register,
  settle,
  viewCommand,
} from "./terminal-view-hub-harness.ts";
import {
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

function resyncCommand(streamId: string) {
  return create(TerminalResyncCommandSchema, {
    viewId: VIEW_A,
    sessionId: SESSION,
    streamId,
  });
}

function schedulerSink(harness: SchedulerHarness): TerminalScreenSocketSink {
  return {
    beginTerminalStream: (sessionId, streamId) =>
      harness.scheduler.beginTerminalStream(harness.ws, sessionId, streamId),
    enqueueTerminalState: (frame, sessionId) =>
      harness.scheduler.enqueueTerminalState(harness.ws, frame, sessionId),
    replaceTerminalSnapshot: (sessionId, streamId, frames) =>
      harness.scheduler.replaceTerminalSnapshot(harness.ws, sessionId, streamId, frames),
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
      dashboardId: DASHBOARD,
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

  test("terminalResync reseeds a dropped browser delivery from the canonical cache", async () => {
    const { hub, snapshotRequests } = makeViewHarness();
    const sink = register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    await settle();
    const streamId = hub.snapshot(SESSION)!.streamId;
    hub.screen.publishFrame(SESSION, fullFrame({ streamId, cols: 80, rows: 24 }));
    expect(sink.snapshots).toHaveLength(1);

    sink.snapshots.length = 0;
    hub.handleResync("socket-a", resyncCommand(streamId));
    await settle();

    expect(sink.snapshots).toHaveLength(1);
    expect(sink.snapshots[0]).toMatchObject({ sessionId: SESSION, streamId });
    expect(snapshotRequests).toEqual([]);
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

    assembling.hub.handleResync("assembling-socket", resyncCommand(assemblingStream));
    assembling.hub.handleResync("assembling-socket", resyncCommand(assemblingStream));
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
