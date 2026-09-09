// Worker-owned terminal pipeline evidence sampler tests.
// They pin bounded target and response admission without terminal-content output.
// The sampler reads live state only; these fixtures use the smallest truthful records.

import { describe, expect, test } from "bun:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { initCellEmitState } from "@roost/shared/cell";
import {
  CoordWorkerUpSchema,
  DTerminalPipelineSnapshotRequestSchema,
  WTerminalPipelineSnapshotSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalPipelineReason,
  TerminalPipelineStage,
  TerminalPipelineTargetSchema,
} from "@roost/shared/proto/wire_pb";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { ChannelId, SessionId } from "@roost/shared/wire";
import {
  TERMINAL_PIPELINE_MAX_RESPONSE_BYTES,
  terminalPipelineSnapshot,
} from "../src/terminal-pipeline-snapshot.ts";
import { SessionManager } from "../src/session-manager.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const WORKER_FP = asWorkerFp("a".repeat(64));
const FIRST_SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const LAST_SESSION_ID = asSessionId("99999999-9999-4999-8999-999999999999");
const MISSING_SESSION_ID = "55555555-5555-4555-8555-555555555555";
const FIRST_CHANNEL_ID = asChannelId(51_001);
const LAST_CHANNEL_ID = asChannelId(51_002);
const STREAM_ID = "22222222-2222-4222-8222-222222222222";

function createManager(): SessionManager {
  const manager = new SessionManager({
    workerFp: WORKER_FP,
    sink: new SessionEventTestSink(),
  });
  addSession(manager, FIRST_SESSION_ID, FIRST_CHANNEL_ID, 11);
  addSession(manager, LAST_SESSION_ID, LAST_CHANNEL_ID, 29);
  return manager;
}

function addSession(
  manager: SessionManager,
  sessionId: SessionId,
  channelId: ChannelId,
  sequence: number,
): void {
  const cellEmit = initCellEmitState("pipeline-grid", STREAM_ID);
  cellEmit.seq = sequence;
  manager.sessions.set(channelId, {
    sessionId,
    channelId,
    cell_emit: cellEmit,
  } as never);
}

function pipelineRequest(
  targets: readonly { sessionId: string; viewId: string }[],
) {
  return create(DTerminalPipelineSnapshotRequestSchema, {
    requestId: "pipeline-request",
    targets: targets.map((target) => create(TerminalPipelineTargetSchema, target)),
  });
}

const detachedTransport = {
  queueFrames: 3,
  queueBytes: 512,
  nativeBufferedBytes: 1024,
  attached: false,
};

describe("terminal pipeline snapshot", () => {
  test("sorts targets, represents a missing session by enum, and omits terminal content", () => {
    const manager = createManager();
    try {
      const snapshot = terminalPipelineSnapshot(manager, pipelineRequest([
        { sessionId: LAST_SESSION_ID, viewId: "view-last" },
        { sessionId: MISSING_SESSION_ID, viewId: "view-missing" },
        { sessionId: FIRST_SESSION_ID, viewId: "view-first" },
      ]), detachedTransport);

      expect(snapshot.sessions.map((session) => session.sessionId)).toEqual([
        FIRST_SESSION_ID,
        MISSING_SESSION_ID,
        LAST_SESSION_ID,
      ]);
      const missing = snapshot.sessions[1];
      expect(missing?.stages).toEqual([expect.objectContaining({
        stage: TerminalPipelineStage.WORKER_STREAM,
        reason: TerminalPipelineReason.SESSION_NOT_FOUND,
      })]);
      const known = snapshot.sessions[0];
      expect(known?.stages).toHaveLength(7);
      expect(known?.stages.every((stage) => stage.histogramBuckets.length <= 16)).toBe(true);
      const link = known?.stages.find((stage) => stage.stage === TerminalPipelineStage.WORKER_COORD_LINK);
      expect(link).toMatchObject({
        reason: TerminalPipelineReason.COORD_LINK_UNAVAILABLE,
        queueFrames: 3n,
        queueBytes: 512n,
        nativeBufferedBytes: 1024n,
      });
      expect(link?.generation).toBe(0n);
      for (const stage of known?.stages ?? []) {
        for (const forbiddenField of ["rows", "spans", "title", "markers", "command", "cwd", "data", "error"]) {
          expect(stage).not.toHaveProperty(forbiddenField);
        }
      }
    } finally {
      manager.sessions.clear();
      manager.dispose();
    }
  });

  test("uses the first session record during provisional respawn overlap", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new SessionEventTestSink(),
    });
    try {
      addSession(manager, FIRST_SESSION_ID, FIRST_CHANNEL_ID, 11);
      addSession(manager, FIRST_SESSION_ID, LAST_CHANNEL_ID, 29);

      expect(manager.getBySessionId(FIRST_SESSION_ID)?.channelId).toBe(FIRST_CHANNEL_ID);
      const snapshot = terminalPipelineSnapshot(manager, pipelineRequest([
        { sessionId: FIRST_SESSION_ID, viewId: "overlap-view" },
      ]), detachedTransport);

      expect(snapshot.sessions).toHaveLength(1);
      expect(snapshot.sessions[0]?.stages).toHaveLength(7);
      expect(snapshot.sessions[0]?.stages.every((stage) => stage.sequence === 11n)).toBe(true);
    } finally {
      manager.sessions.clear();
      manager.dispose();
    }
  });

  test("limits request admission to sixty-four targets", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new SessionEventTestSink(),
    });
    try {
      const snapshot = terminalPipelineSnapshot(manager, pipelineRequest(
        Array.from({ length: 65 }, (_, index) => ({
          sessionId: `missing-${String(index).padStart(2, "0")}`,
          viewId: `view-${String(index).padStart(2, "0")}`,
        })),
      ), detachedTransport);

      expect(snapshot.sessions).toHaveLength(64);
      expect(snapshot.droppedTargets).toBe(1);
      expect(snapshot.droppedRecords).toBe(0);
      expect(snapshot.sessions.every((session) =>
        session.stages[0]?.reason === TerminalPipelineReason.SESSION_NOT_FOUND,
      )).toBe(true);
      expect(toBinary(WTerminalPipelineSnapshotSchema, snapshot).byteLength)
        .toBeLessThanOrEqual(TERMINAL_PIPELINE_MAX_RESPONSE_BYTES);
    } finally {
      manager.dispose();
    }
  });

  test("drops oversized response records deterministically while preserving the encoded response bound", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new SessionEventTestSink(),
    });
    try {
      const targets = Array.from({ length: 64 }, (_, index) => ({
        sessionId: `session-${String(index).padStart(3, "0")}`.padEnd(512, "s"),
        viewId: `view-${String(index).padStart(3, "0")}`.padEnd(512, "v"),
      }));
      const request = pipelineRequest(targets);
      const first = terminalPipelineSnapshot(manager, request, detachedTransport);
      const second = terminalPipelineSnapshot(manager, request, detachedTransport);

      expect(first.sessions.length).toBeGreaterThan(0);
      expect(first.sessions.length).toBeLessThan(64);
      expect(first.droppedTargets).toBe(0);
      expect(first.droppedRecords).toBeGreaterThan(0);
      expect(first.sessions.map((session) => session.sessionId)).toEqual(
        targets.slice(0, first.sessions.length).map((target) => target.sessionId),
      );
      expect(second.sessions.map((session) => session.sessionId)).toEqual(
        first.sessions.map((session) => session.sessionId),
      );
      expect(second.droppedRecords).toBe(first.droppedRecords);
      expect(toBinary(WTerminalPipelineSnapshotSchema, first).byteLength)
        .toBeLessThanOrEqual(TERMINAL_PIPELINE_MAX_RESPONSE_BYTES);
      const envelope = create(CoordWorkerUpSchema, {
        frame: { case: "terminalPipelineSnapshot", value: first },
      });
      expect(toBinary(CoordWorkerUpSchema, envelope).byteLength)
        .toBeLessThanOrEqual(TERMINAL_PIPELINE_MAX_RESPONSE_BYTES);
    } finally {
      manager.dispose();
    }
  });

  test("counts an oversized target identifier as a dropped record", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new SessionEventTestSink(),
    });
    try {
      const snapshot = terminalPipelineSnapshot(manager, pipelineRequest([
        { sessionId: "s".repeat(513), viewId: "oversized-view" },
        { sessionId: MISSING_SESSION_ID, viewId: "valid-view" },
      ]), detachedTransport);

      expect(snapshot.sessions.map((session) => session.sessionId)).toEqual([MISSING_SESSION_ID]);
      expect(snapshot.droppedTargets).toBe(0);
      expect(snapshot.droppedRecords).toBe(1);
    } finally {
      manager.dispose();
    }
  });
});
