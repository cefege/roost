// Pins the direct-attachment protobuf surface and browser-safe carrier contract.
// Browser, coordinator, and worker import these names instead of deriving a
// second attachment frame, grant authority, route, channel, or chunk policy.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  ATTACHMENT_TRANSFER_ACK_DEADLINE_MS,
  ATTACHMENT_TRANSFER_ACTIVE_MAX_MS,
  ATTACHMENT_TRANSFER_CHUNK_SHA256_HEX_LENGTH,
  ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
  ATTACHMENT_TRANSFER_ERROR_REASONS,
  ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS,
  ATTACHMENT_TRANSFER_GRANT_TTL_MS,
  ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
  ATTACHMENT_TRANSFER_IDLE_MS,
  ATTACHMENT_TRANSFER_LOOPBACK_PATH,
  ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL,
  ATTACHMENT_TRANSFER_MAX_CHUNKS_IN_FLIGHT,
  ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS,
  ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE,
  ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS,
  ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS,
  ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL,
  ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS,
  ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS,
  isAttachmentTransferChunkSha256,
} from "../src/attachment-transfer.ts";
import {
  AttachmentTransferAckSchema,
  AttachmentTransferChunkSchema,
  AttachmentTransferClientFrameSchema,
  AttachmentTransferClosedSchema,
  AttachmentTransferHelloSchema,
  AttachmentTransferReadySchema,
  AttachmentTransferServerFrameSchema,
  AttachmentTransferStatusRequestSchema,
  AttachmentTransferStatusSchema,
} from "../src/gen/roost/v1/attachment_transfer_pb.ts";
import {
  AttachmentsDirectStatusRequestSchema,
  AttachmentsDirectStatusResponseSchema,
  AttachmentsGrantDirectRequestSchema,
  AttachmentsGrantDirectResponseSchema,
  CoordinatorService,
  SessionsGrantLocalTerminalResponseSchema,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  DAttachmentDirectStatusRequestSchema,
  DLocalAttachmentGrantRevokeSchema,
  DLocalAttachmentGrantSchema,
  DLocalAttachmentPeerCancelSchema,
  DLocalAttachmentPeerOfferSchema,
  WAttachmentDirectStatusSchema,
  WLocalAttachmentPeerAnswerSchema,
  WLocalAttachmentPeerErrorSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";

const CHUNK_SHA256 = "a".repeat(64);

function attachmentHello() {
  return create(AttachmentTransferHelloSchema, {
    grantId: "g",
    secret: "s",
    tabId: "t",
    deviceFingerprint: "d",
    sessionId: "i",
    uploadId: "u",
    filename: "f",
    shortPath: true,
    totalBytes: 9n,
    peerId: "p",
    workerEpoch: "e",
  });
}

function attachmentStatus() {
  return create(AttachmentTransferStatusSchema, {
    uploadId: "upload",
    nextSeq: 4,
    bytesReceived: 512n,
    lastChunkSha256: CHUNK_SHA256,
    committed: true,
    absPath: "/attachment",
    error: "",
  });
}

describe("attachment transfer protocol", () => {
  test("keeps exact hello, integrity fields, and direct frame cases stable", () => {
    const hello = attachmentHello();
    expect([...toBinary(AttachmentTransferHelloSchema, hello)]).toEqual([
      10, 1, 103, 18, 1, 115, 26, 1, 116, 34, 1, 100, 42, 1, 105,
      50, 1, 117, 58, 1, 102, 64, 1, 72, 9, 82, 1, 112, 90, 1, 101,
    ]);

    const chunk = create(AttachmentTransferChunkSchema, {
      uploadId: "u", seq: 3, data: new Uint8Array([1, 2]), last: true,
      offset: 2n, chunkSha256: CHUNK_SHA256,
    });
    expect([...toBinary(AttachmentTransferChunkSchema, chunk).subarray(0, 15)]).toEqual([
      10, 1, 117, 16, 3, 26, 2, 1, 2, 32, 1, 40, 2, 50, 64,
    ]);
    expect(fromBinary(AttachmentTransferChunkSchema, toBinary(AttachmentTransferChunkSchema, chunk))).toMatchObject({
      offset: 2n,
      chunkSha256: CHUNK_SHA256,
    });

    const clientFrames = [
      create(AttachmentTransferClientFrameSchema, { frame: { case: "hello", value: hello } }),
      create(AttachmentTransferClientFrameSchema, { frame: { case: "chunk", value: chunk } }),
      create(AttachmentTransferClientFrameSchema, {
        frame: { case: "statusRequest", value: create(AttachmentTransferStatusRequestSchema, { uploadId: "upload" }) },
      }),
    ];
    expect(clientFrames.map((frame) => fromBinary(
      AttachmentTransferClientFrameSchema,
      toBinary(AttachmentTransferClientFrameSchema, frame),
    ).frame.case)).toEqual(["hello", "chunk", "statusRequest"]);

    const status = attachmentStatus();
    const serverFrames = [
      create(AttachmentTransferServerFrameSchema, {
        frame: { case: "ready", value: create(AttachmentTransferReadySchema, {
          workerFingerprint: "worker", workerEpoch: "epoch", sessionId: "session", uploadId: "upload",
        }) },
      }),
      create(AttachmentTransferServerFrameSchema, {
        frame: { case: "ack", value: create(AttachmentTransferAckSchema, {
          uploadId: "upload", seq: 3, bytesReceived: 2n, absPath: "/attachment", error: "",
          chunkSha256: CHUNK_SHA256,
        }) },
      }),
      create(AttachmentTransferServerFrameSchema, {
        frame: { case: "closed", value: create(AttachmentTransferClosedSchema, { reason: "complete" }) },
      }),
      create(AttachmentTransferServerFrameSchema, { frame: { case: "status", value: status } }),
    ];
    expect(serverFrames.map((frame) => fromBinary(
      AttachmentTransferServerFrameSchema,
      toBinary(AttachmentTransferServerFrameSchema, frame),
    ).frame.case)).toEqual(["ready", "ack", "closed", "status"]);
  });

  test("keeps direct attachment grants and status separate from terminal grants", () => {
    expect(CoordinatorService.methods.map((method) => method.localName)).toEqual(expect.arrayContaining([
      "attachmentsGrantDirect",
      "attachmentsDirectStatus",
      "sessionsNegotiateAttachmentPeer",
      "attachFileChunk",
      "attachmentProbe",
    ]));
    expect(create(SessionsGrantLocalTerminalResponseSchema, {})).not.toHaveProperty("attachmentPeerSupported");

    const grantRequest = create(AttachmentsGrantDirectRequestSchema, {
      sessionId: "session", workerFp: "worker", tabId: "tab", uploadId: "upload",
      filename: "file.txt", shortPath: false, totalBytes: 512n,
    });
    expect(fromBinary(
      AttachmentsGrantDirectRequestSchema,
      toBinary(AttachmentsGrantDirectRequestSchema, grantRequest),
    )).toMatchObject({ uploadId: "upload", totalBytes: 512n });

    const grantResponse = create(AttachmentsGrantDirectResponseSchema, {
      grantId: "grant", secret: "secret", ttlMs: 60_000, workerEpoch: "epoch",
      peerSupported: true, stunUrls: ["stun:stun.example:3478"],
    });
    expect(fromBinary(
      AttachmentsGrantDirectResponseSchema,
      toBinary(AttachmentsGrantDirectResponseSchema, grantResponse),
    )).toMatchObject({ peerSupported: true, workerEpoch: "epoch" });

    const statusRequest = create(AttachmentsDirectStatusRequestSchema, {
      sessionId: "session", uploadId: "upload",
    });
    expect(fromBinary(
      AttachmentsDirectStatusRequestSchema,
      toBinary(AttachmentsDirectStatusRequestSchema, statusRequest),
    )).toMatchObject({ sessionId: "session", uploadId: "upload" });

    const response = fromBinary(
      AttachmentsDirectStatusResponseSchema,
      toBinary(AttachmentsDirectStatusResponseSchema, create(AttachmentsDirectStatusResponseSchema, {
        status: attachmentStatus(),
      })),
    );
    expect(response.status).toMatchObject({ committed: true, lastChunkSha256: CHUNK_SHA256 });
  });

  test("assigns attachment peer, grant, and status control frames distinct tags", () => {
    const offer = create(DLocalAttachmentPeerOfferSchema, {
      requestId: "request", connectionGeneration: "connection", workerEpoch: "epoch",
      grantId: "grant", peerId: "peer", deviceFingerprint: "device", tabId: "tab",
      offerSdp: "offer", budgetMs: 8_000, stunUrls: ["stun:stun.example:3478"],
    });
    const grant = create(DLocalAttachmentGrantSchema, {
      requestId: "request", grantId: "grant", secretSha256: CHUNK_SHA256,
      sessionId: "session", uploadId: "upload", filename: "file.txt", shortPath: false,
      totalBytes: 512n, deviceFingerprint: "device", tabId: "tab", ttlMs: 60_000, workerEpoch: "epoch",
    });
    const downstreamFrames = [
      create(CoordWorkerDownSchema, { frame: { case: "localAttachmentPeerOffer", value: offer } }),
      create(CoordWorkerDownSchema, {
        frame: { case: "localAttachmentPeerCancel", value: create(DLocalAttachmentPeerCancelSchema, {
          requestId: "request", connectionGeneration: "connection", workerEpoch: "epoch", peerId: "peer",
        }) },
      }),
      create(CoordWorkerDownSchema, { frame: { case: "localAttachmentGrant", value: grant } }),
      create(CoordWorkerDownSchema, {
        frame: { case: "localAttachmentGrantRevoke", value: create(DLocalAttachmentGrantRevokeSchema, {
          deviceFingerprint: "device",
        }) },
      }),
      create(CoordWorkerDownSchema, {
        frame: { case: "attachmentDirectStatusRequest", value: create(DAttachmentDirectStatusRequestSchema, {
          requestId: "request", sessionId: "session", uploadId: "upload",
        }) },
      }),
    ];
    expect(downstreamFrames.map((frame) => {
      const encoded = toBinary(CoordWorkerDownSchema, frame);
      return { tag: [...encoded.subarray(0, 2)], frame: fromBinary(CoordWorkerDownSchema, encoded).frame.case };
    })).toEqual([
      { tag: [218, 1], frame: "localAttachmentPeerOffer" },
      { tag: [226, 1], frame: "localAttachmentPeerCancel" },
      { tag: [234, 1], frame: "localAttachmentGrant" },
      { tag: [250, 1], frame: "localAttachmentGrantRevoke" },
      { tag: [130, 2], frame: "attachmentDirectStatusRequest" },
    ]);

    const upstreamFrames = [
      create(CoordWorkerUpSchema, {
        frame: { case: "localAttachmentPeerAnswer", value: create(WLocalAttachmentPeerAnswerSchema, {
          requestId: "request", connectionGeneration: "connection", workerEpoch: "epoch",
          peerId: "peer", answerSdp: "answer",
        }) },
      }),
      create(CoordWorkerUpSchema, {
        frame: { case: "localAttachmentPeerError", value: create(WLocalAttachmentPeerErrorSchema, {
          requestId: "request", connectionGeneration: "connection", workerEpoch: "epoch",
          peerId: "peer", reason: "capacity",
        }) },
      }),
      create(CoordWorkerUpSchema, {
        frame: { case: "attachmentDirectStatus", value: create(WAttachmentDirectStatusSchema, {
          requestId: "request", status: attachmentStatus(),
        }) },
      }),
    ];
    expect(upstreamFrames.map((frame) => {
      const encoded = toBinary(CoordWorkerUpSchema, frame);
      return { tag: [...encoded.subarray(0, 2)], frame: fromBinary(CoordWorkerUpSchema, encoded).frame.case };
    })).toEqual([
      { tag: [218, 1], frame: "localAttachmentPeerAnswer" },
      { tag: [226, 1], frame: "localAttachmentPeerError" },
      { tag: [234, 1], frame: "attachmentDirectStatus" },
    ]);
  });

  test("centralizes direct integrity, grant, loopback, channel, and status bounds", () => {
    expect(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES).toBe(512 * 1024);
    expect(ATTACHMENT_TRANSFER_CHUNK_SHA256_HEX_LENGTH).toBe(64);
    expect(isAttachmentTransferChunkSha256(CHUNK_SHA256)).toBe(true);
    expect(isAttachmentTransferChunkSha256(CHUNK_SHA256.toUpperCase())).toBe(false);
    expect(ATTACHMENT_TRANSFER_ERROR_REASONS).toEqual(expect.arrayContaining([
      "upload_not_found",
      "chunk_offset_mismatch",
      "chunk_sha256_mismatch",
    ]));
    expect(ATTACHMENT_TRANSFER_MAX_CHUNKS_IN_FLIGHT).toBe(1);
    expect(ATTACHMENT_TRANSFER_LOOPBACK_PATH).toBe("/ws/local-attachment-transfer");
    expect(ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL).toBe("roost-local-attachment-transfer-v1");
    expect(ATTACHMENT_TRANSFER_GRANT_TTL_MS).toBe(60_000);
    expect(ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS).toBe(8_000);
    expect(ATTACHMENT_TRANSFER_ACTIVE_MAX_MS).toBe(12 * 60 * 60_000);
    expect(ATTACHMENT_TRANSFER_IDLE_MS).toBe(5 * 60_000);
    expect(ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS).toBe(3_000);
    expect(ATTACHMENT_TRANSFER_ACK_DEADLINE_MS).toBe(15_000);
    expect(ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS).toBe(8_000);
    expect(ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS).toBe(64);
    expect(ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE).toBe(8);
    expect(ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS).toBe(64);
    expect(ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS).toBe(15_000);
    expect(ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS).toBe(8_000);
    expect(ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS).toEqual([
      {
        lane: "control", id: 0, label: "roost-attachment-control-v1", ordered: true,
        protocol: ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL,
      },
      {
        lane: "data", id: 1, label: "roost-attachment-data-v1", ordered: true,
        protocol: ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL,
      },
    ]);
  });
});
