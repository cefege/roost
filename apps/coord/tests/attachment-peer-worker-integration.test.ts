// Focused worker-link coverage for attachment peer capability acknowledgement,
// typed signaling results, and typed durable receipt-status dispatch. The
// dispatcher receives the exact current WorkerHandle after existing connection
// fencing, while no direct attachment bytes cross this coordinator test path.

import { afterEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AttachmentTransferStatusSchema,
} from "@roost/shared/proto/attachment_transfer_pb";
import {
  CoordWorkerUpSchema,
  WHelloSchema,
  WAttachmentDirectStatusSchema,
  WLocalAttachmentPeerAnswerSchema,
  WLocalAttachmentPeerErrorSchema,
  type CoordWorkerDown,
  type WAttachmentDirectStatus,
  type WLocalAttachmentPeerAnswer,
  type WLocalAttachmentPeerError,
} from "@roost/shared/proto/worker_transport_pb";
import { ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY } from "@roost/shared/attachment-transfer";
import { makeWorkerConn, type WorkerConn } from "../src/connect/worker-conn.ts";
import { makeWorkerFrameDispatcher } from "../src/connect/worker-frame-dispatch.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-conn-types.ts";
import type { WorkerHandle } from "../src/connect/worker-registry.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";

const WORKER_FP = "f".repeat(64);
const WORKER_EPOCH = "attachment-worker-epoch";
let connection: WorkerConn | null = null;

afterEach(() => {
  connection?.close();
  connection = null;
  __setConnectWorkerForTest(WORKER_FP, null);
});

test("acknowledges attachment peer capability only while its separate result owner is installed", async () => {
  const sent: CoordWorkerDown[] = [];
  connection = makeWorkerConn({
    cfg: { terminalPeerEnabled: true },
    attachmentPeerNegotiations: {
      acceptAnswer: () => false,
      acceptError: () => false,
      cancelForWorkerHandle() {},
    },
  } as unknown as WorkerServiceDeps, { fingerprint: WORKER_FP }, (frame) => {
    sent.push(frame);
    return 1;
  }, () => {});
  await connection.handleUpstream(create(CoordWorkerUpSchema, {
    frame: {
      case: "hello",
      value: create(WHelloSchema, {
        workerFp: WORKER_FP,
        version: "test",
        processEpoch: WORKER_EPOCH,
        capabilities: [ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY],
      }),
    },
  }));

  const acknowledgement = sent.at(-1);
  if (acknowledgement?.frame.case !== "helloAck") throw new Error("expected hello acknowledgement");
  expect(acknowledgement.frame.value.capabilities).toEqual([ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY]);
});

test("dispatches attachment peer and status frames through the exact current handle", () => {
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: WORKER_EPOCH,
    connectionGeneration: "attachment-worker-connection",
    capabilities: new Set([ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY]),
    revoked: false,
    ready: true,
    send: () => 1,
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  const answers: string[] = [];
  const errors: string[] = [];
  const statuses: string[] = [];
  const dispatcher = makeWorkerFrameDispatcher({
    deps: {
      attachmentPeerNegotiations: {
        acceptAnswer(source: WorkerHandle, answer: WLocalAttachmentPeerAnswer) {
          expect(source).toBe(worker);
          answers.push(answer.requestId);
          return true;
        },
        acceptError(source: WorkerHandle, error: WLocalAttachmentPeerError) {
          expect(source).toBe(worker);
          errors.push(error.requestId);
          return true;
        },
        cancelForWorkerHandle() {},
      },
      attachmentDirectStatusResults: {
        acceptStatus(source: WorkerHandle, status: WAttachmentDirectStatus) {
          expect(source).toBe(worker);
          statuses.push(status.requestId);
          return true;
        },
        cancelForWorkerHandle() {},
      },
    } as unknown as WorkerServiceDeps,
    callerFingerprint: WORKER_FP,
    requestClose() {},
    getWorkerFp: () => WORKER_FP,
    getWorkerHandle: () => worker,
    isSnapshotReady: () => true,
    isCurrentGeneration: () => true,
    fenced: () => false,
    sendBestEffort: () => true,
    markSnapshotReady: () => false,
    scheduleRespawn() {},
  });
  const answer = create(CoordWorkerUpSchema, {
    frame: {
      case: "localAttachmentPeerAnswer",
      value: create(WLocalAttachmentPeerAnswerSchema, {
        requestId: "answer-request",
        connectionGeneration: worker.connectionGeneration,
        workerEpoch: WORKER_EPOCH,
        peerId: "00000000-0000-4000-8000-000000000020",
        answerSdp: "bounded-by-owner",
      }),
    },
  });
  const error = create(CoordWorkerUpSchema, {
    frame: {
      case: "localAttachmentPeerError",
      value: create(WLocalAttachmentPeerErrorSchema, {
        requestId: "error-request",
        connectionGeneration: worker.connectionGeneration,
        workerEpoch: WORKER_EPOCH,
        peerId: "00000000-0000-4000-8000-000000000021",
        reason: "ice_failed",
      }),
    },
  });
  const status = create(CoordWorkerUpSchema, {
    frame: {
      case: "attachmentDirectStatus",
      value: create(WAttachmentDirectStatusSchema, {
        requestId: "status-request",
        status: create(AttachmentTransferStatusSchema, {
          uploadId: "00000000-0000-4000-8000-000000000022",
          nextSeq: 1,
          bytesReceived: 512n,
          lastChunkSha256: "a".repeat(64),
        }),
      }),
    },
  });

  expect(dispatcher.handleLiveFrame(answer)).toBe(true);
  expect(dispatcher.handleLiveFrame(error)).toBe(true);
  expect(dispatcher.handleLiveFrame(status)).toBe(true);
  expect(answers).toEqual(["answer-request"]);
  expect(errors).toEqual(["error-request"]);
  expect(statuses).toEqual(["status-request"]);
});
