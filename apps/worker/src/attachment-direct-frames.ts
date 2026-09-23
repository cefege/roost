// Server-frame encoding for direct attachment ports. AttachmentDirectSockets
// decides what to send and when; this module only builds each protobuf frame
// and hands it to the port's control lane, reporting refusal so the caller can
// retire the route.

import { create, toBinary } from "@bufbuild/protobuf";
import {
  AttachmentTransferAckSchema,
  AttachmentTransferClosedSchema,
  AttachmentTransferReadySchema,
  AttachmentTransferServerFrameSchema,
  AttachmentTransferStatusSchema,
  type AttachmentTransferServerFrame,
} from "@roost/shared/proto/attachment_transfer_pb";
import type { AttachmentOperationStatus } from "./attachment-operation-receipts.ts";
import type { AttachmentTransferPort } from "./attachment-transfer-port.ts";

export interface AttachmentReadyFields {
  readonly workerFingerprint: string;
  readonly workerEpoch: string;
  readonly sessionId: string;
  readonly uploadId: string;
}

/** An operation receipt satisfies this shape; a failure ACK supplies it directly. */
export interface AttachmentAckFields {
  readonly seq: number;
  readonly bytesReceived: number;
  readonly chunkSha256: string;
}

export function sendAttachmentReady(port: AttachmentTransferPort, ready: AttachmentReadyFields): boolean {
  return sendServerFrame(port, { case: "ready", value: create(AttachmentTransferReadySchema, ready) });
}

export function sendAttachmentAck(
  port: AttachmentTransferPort,
  uploadId: string,
  ack: AttachmentAckFields,
  absPath: string,
  error: string,
): boolean {
  return sendServerFrame(port, {
    case: "ack",
    value: create(AttachmentTransferAckSchema, {
      uploadId,
      seq: ack.seq,
      bytesReceived: BigInt(ack.bytesReceived),
      absPath,
      error,
      chunkSha256: ack.chunkSha256,
    }),
  });
}

export function sendAttachmentStatus(port: AttachmentTransferPort, status: AttachmentOperationStatus): boolean {
  return sendServerFrame(port, {
    case: "status",
    value: create(AttachmentTransferStatusSchema, {
      ...status,
      bytesReceived: BigInt(status.bytesReceived),
    }),
  });
}

export function sendAttachmentClosed(port: AttachmentTransferPort, reason: string): boolean {
  return sendServerFrame(port, { case: "closed", value: create(AttachmentTransferClosedSchema, { reason }) });
}

function sendServerFrame(port: AttachmentTransferPort, frame: AttachmentTransferServerFrame["frame"]): boolean {
  try {
    const bytes = toBinary(AttachmentTransferServerFrameSchema, create(AttachmentTransferServerFrameSchema, { frame }));
    return port.send(bytes, "control") !== "refused";
  } catch {
    return false;
  }
}
