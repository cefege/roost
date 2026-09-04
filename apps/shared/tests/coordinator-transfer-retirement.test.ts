// Owns the public protocol contract for retired cross-worker transfers.
// Bun discovers it to keep removed RPCs, commands, and result frames unavailable.
// It also pins the attachment RPCs that continue to move files to terminals.

import { describe, expect, test } from "bun:test";
import * as coordinator from "../src/gen/roost/v1/coordinator_pb.ts";
import * as workerTransport from "../src/gen/roost/v1/worker_transport_pb.ts";
import { ClientControlFrame } from "../src/wire/control.ts";
import { CoordWorkerUpstream } from "../src/wire/coord-worker.ts";

const retiredRpcNames = [
  ["Transfers", "Start"].join(""),
  ["Transfers", "Output"].join(""),
];
const retiredLocalNames = [
  ["transfers", "Start"].join(""),
  ["transfers", "Output"].join(""),
];
const retiredSchemaExports = [
  ["Transfers", "StartRequestSchema"].join(""),
  ["Transfers", "StartResponseSchema"].join(""),
  ["Transfers", "OutputRequestSchema"].join(""),
  ["Transfers", "OutputFrameSchema"].join(""),
];
const retiredWorkerCommand = ["start", "transfer"].join("-");
const retiredWorkerFrameNames = [
  ["transfer", "line"].join("-"),
  ["transfer", "done"].join("-"),
];
const retiredWorkerSchemaExports = [
  ["WTransfer", "LineSchema"].join(""),
  ["WTransfer", "DoneSchema"].join(""),
];

const coordinatorRuntimeExports = coordinator as unknown as Record<string, unknown>;
const workerTransportRuntimeExports = workerTransport as unknown as Record<string, unknown>;
const serviceRpcNames = coordinator.CoordinatorService.methods.map((method) => method.name);
const serviceLocalNames = coordinator.CoordinatorService.methods.map((method) => method.localName);

describe("cross-worker transfer protocol retirement", () => {
  test("does not expose the retired cross-worker runtime API", () => {
    for (const name of retiredRpcNames) expect(serviceRpcNames).not.toContain(name);
    for (const name of retiredLocalNames) expect(serviceLocalNames).not.toContain(name);
    for (const name of retiredSchemaExports) expect(coordinatorRuntimeExports).not.toHaveProperty(name);
  });

  test("does not expose worker commands or result frames", () => {
    for (const name of retiredWorkerSchemaExports) {
      expect(workerTransportRuntimeExports).not.toHaveProperty(name);
    }
    expect(ClientControlFrame.safeParse({
      kind: retiredWorkerCommand,
      job_id: "job",
      src_path: "/source",
      dst_host: "worker.example",
      dst_path: "/destination",
    }).success).toBe(false);
    for (const kind of retiredWorkerFrameNames) {
      expect(CoordWorkerUpstream.safeParse({ kind, job_id: "job" }).success).toBe(false);
    }
  });

  test("preserves attachment RPCs and runtime schemas", () => {
    expect(serviceLocalNames).toEqual(expect.arrayContaining([
      "attachFileChunk",
      "attachmentProbe",
      "listAttachments",
      "deleteAttachment",
    ]));
    for (const name of [
      "AttachFileChunkRequestSchema",
      "AttachmentProbeRequestSchema",
      "ListAttachmentsRequestSchema",
      "DeleteAttachmentRequestSchema",
    ]) {
      expect(coordinatorRuntimeExports).toHaveProperty(name);
    }
  });
});
