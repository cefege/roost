// Typed terminal-pipeline CoordLink route tests.
// They keep this evidence plane out of the legacy generic RPC payload path.
// Sampling itself is covered by terminal-pipeline-snapshot.test.ts.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DTerminalPipelineSnapshotRequestSchema,
  WTerminalPipelineSnapshotSchema,
  type DTerminalPipelineSnapshotRequest,
} from "@roost/shared/proto/worker_transport_pb";
import { TerminalPipelineTargetSchema } from "@roost/shared/proto/wire_pb";
import { asWorkerFp } from "@roost/shared/wire";
import { frameToProto } from "../src/transport/coord-link-codec.ts";
import { createCoordLinkDownstream } from "../src/transport/coord-link-downstream.ts";
import { buildCoordLinkDeps, type CoordLinkRefs } from "../src/coord-link-deps.ts";
import { SessionManager } from "../src/session-manager.ts";
import type {
  CoordLink,
  CoordLinkDeps,
  CoordLinkOutbox,
  UpstreamFrame,
} from "../src/transport/coord-link-types.ts";
import type { SessionEventStore } from "../src/transport/session-event-store.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const VIEW_ID = "22222222-2222-4222-8222-222222222222";
const WORKER_FP = asWorkerFp("b".repeat(64));

function pipelineRequest(): DTerminalPipelineSnapshotRequest {
  return create(DTerminalPipelineSnapshotRequestSchema, {
    requestId: "pipeline-request",
    targets: [create(TerminalPipelineTargetSchema, {
      sessionId: SESSION_ID,
      viewId: VIEW_ID,
    })],
  });
}

describe("CoordLink terminal pipeline route", () => {
  test("encodes the typed upstream frame without the generic RPC envelope", () => {
    const snapshot = create(WTerminalPipelineSnapshotSchema, {
      requestId: "pipeline-request",
      sessions: [],
      droppedTargets: 1,
      droppedRecords: 2,
    });
    const frame = frameToProto({
      kind: "terminal-pipeline-snapshot",
      snapshot,
    } satisfies UpstreamFrame);

    expect(frame?.frame.case).toBe("terminalPipelineSnapshot");
    if (!frame || frame.frame.case !== "terminalPipelineSnapshot") {
      throw new Error("typed terminal pipeline response was not encoded");
    }
    expect(frame.frame.value).toBe(snapshot);
  });

  test("dispatches the typed downstream request directly to its owner", () => {
    const request = pipelineRequest();
    let received: DTerminalPipelineSnapshotRequest | undefined;
    const socket = {} as WebSocket;
    const downstream = createCoordLinkDownstream({
      onTerminalPipelineSnapshot: (
        next: DTerminalPipelineSnapshotRequest,
      ) => { received = next; },
    } as unknown as CoordLinkDeps, {
      send: () => true,
      activeSocket: () => socket,
    } as unknown as CoordLinkOutbox);

    downstream.handleDownstream(create(CoordWorkerDownSchema, {
      frame: { case: "terminalPipelineSnapshot", value: request },
    }), false, socket);

    expect(received).toBe(request);
  });

  test("builds a typed snapshot owner that samples and sends one response", () => {
    const manager = new SessionManager({
      workerFp: WORKER_FP,
      sink: new SessionEventTestSink(),
    });
    let sent: UpstreamFrame | undefined;
    const link = {
      pipelineState: () => ({
        queueFrames: 0,
        queueBytes: 0,
        nativeBufferedBytes: 0,
        attached: false,
      }),
      send: (frame: UpstreamFrame) => {
        sent = frame;
        return true;
      },
    } as unknown as CoordLink;
    const refs: CoordLinkRefs = {
      link,
      sessionMgr: manager,
      agentRegistry: null,
      agentDetector: null,
      acquireKeeperUpdateBoundary: null,
    };
    try {
      const deps = buildCoordLinkDeps({
        coordHttpUrl: "https://coord.invalid",
        workerFp: WORKER_FP,
        mintJwt: async () => "",
        sessionEventStore: {} as SessionEventStore,
        refs,
      });
      deps.onTerminalPipelineSnapshot?.(pipelineRequest());

      expect(sent?.kind).toBe("terminal-pipeline-snapshot");
      if (!sent || sent.kind !== "terminal-pipeline-snapshot") {
        throw new Error("terminal pipeline owner did not send a typed response");
      }
      expect(sent.snapshot).toMatchObject({
        requestId: "pipeline-request",
        sessions: [expect.objectContaining({
          sessionId: SESSION_ID,
          viewId: VIEW_ID,
        })],
      });
    } finally {
      manager.dispose();
    }
  });
});
