// Typed terminal pipeline evidence protobuf contract.
// Pins its additive worker-frame tags and the bounded typed record shape.
// Sampling ownership remains outside this shared wire test.

import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DTerminalPipelineSnapshotRequestSchema,
  WTerminalPipelineSnapshotSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";
import {
  TerminalPipelineReason,
  TerminalPipelineSessionSnapshotSchema,
  TerminalPipelineStage,
  TerminalPipelineStageSnapshotSchema,
  TerminalPipelineTargetSchema,
} from "../src/gen/roost/v1/wire_pb.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const VIEW_ID = "22222222-2222-4222-8222-222222222222";

describe("terminal pipeline protobuf contract", () => {
  test("round-trips additive typed request and response frames", () => {
    const target = create(TerminalPipelineTargetSchema, {
      sessionId: SESSION_ID,
      viewId: VIEW_ID,
    });
    const request = create(DTerminalPipelineSnapshotRequestSchema, {
      requestId: "pipeline-request",
      targets: [target],
    });
    const downstream = create(CoordWorkerDownSchema, {
      frame: { case: "terminalPipelineSnapshot", value: request },
    });
    const stage = create(TerminalPipelineStageSnapshotSchema, {
      stage: TerminalPipelineStage.WORKER_COORD_LINK,
      reason: TerminalPipelineReason.NATIVE_BUFFERED,
      generation: 7n,
      streamId: "33333333-3333-4333-8333-333333333333",
      sequence: 41n,
      queueFrames: 3n,
      queueBytes: 512n,
      nativeBufferedBytes: 1024n,
      oldestAgeMs: 64n,
      count: 3n,
      histogramBuckets: Array<bigint>(16).fill(0n),
    });
    const response = create(WTerminalPipelineSnapshotSchema, {
      requestId: request.requestId,
      sessions: [create(TerminalPipelineSessionSnapshotSchema, {
        sessionId: target.sessionId,
        viewId: target.viewId,
        stages: [stage],
      })],
      droppedTargets: 2,
      droppedRecords: 1,
    });
    const upstream = create(CoordWorkerUpSchema, {
      frame: { case: "terminalPipelineSnapshot", value: response },
    });

    const downstreamBytes = toBinary(CoordWorkerDownSchema, downstream);
    const upstreamBytes = toBinary(CoordWorkerUpSchema, upstream);
    expect([...downstreamBytes.subarray(0, 2)]).toEqual([0x8a, 0x01]);
    expect([...upstreamBytes.subarray(0, 2)]).toEqual([0x9a, 0x01]);

    const decodedDownstream = fromBinary(CoordWorkerDownSchema, downstreamBytes);
    expect(decodedDownstream.frame.case).toBe("terminalPipelineSnapshot");
    if (decodedDownstream.frame.case !== "terminalPipelineSnapshot") {
      throw new Error("terminal pipeline downstream frame was lost");
    }
    expect(decodedDownstream.frame.value).toMatchObject({
      requestId: "pipeline-request",
      targets: [{ sessionId: SESSION_ID, viewId: VIEW_ID }],
    });

    const decodedUpstream = fromBinary(CoordWorkerUpSchema, upstreamBytes);
    expect(decodedUpstream.frame.case).toBe("terminalPipelineSnapshot");
    if (decodedUpstream.frame.case !== "terminalPipelineSnapshot") {
      throw new Error("terminal pipeline upstream frame was lost");
    }
    expect(decodedUpstream.frame.value).toMatchObject({
      requestId: "pipeline-request",
      droppedTargets: 2,
      droppedRecords: 1,
    });
    expect(decodedUpstream.frame.value.sessions[0]?.stages[0]?.histogramBuckets)
      .toHaveLength(16);
  });
});
