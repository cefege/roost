// Owns the generated coordinator contract for retired cross-worker transfers.
// Bun discovers it to keep removed methods and message schemas unavailable.
// It also pins the attachment RPCs that continue to move files to terminals.

import { describe, expect, test } from "bun:test";
import * as coordinator from "../src/gen/roost/v1/coordinator_pb.ts";

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

const runtimeExports = coordinator as unknown as Record<string, unknown>;
const serviceRpcNames = coordinator.CoordinatorService.methods.map((method) => method.name);
const serviceLocalNames = coordinator.CoordinatorService.methods.map((method) => method.localName);

describe("coordinator transfer protobuf contract", () => {
  test("does not expose the retired cross-worker runtime API", () => {
    for (const name of retiredRpcNames) expect(serviceRpcNames).not.toContain(name);
    for (const name of retiredLocalNames) expect(serviceLocalNames).not.toContain(name);
    for (const name of retiredSchemaExports) expect(runtimeExports).not.toHaveProperty(name);
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
      expect(runtimeExports).toHaveProperty(name);
    }
  });
});
