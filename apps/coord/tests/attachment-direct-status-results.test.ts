// Focused coordinator tests for typed attachment receipt-status correlation.
// They prove exact worker handle fencing and reject missing/mismatched optional
// status payloads without sending attachment bytes through the coordinator.
// Timers are deterministic injected clocks, never wall-clock sleeps.

import { afterEach, expect, test } from "bun:test";
import { Code } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  AttachmentTransferStatusSchema,
} from "@roost/protocol/proto/attachment_transfer_pb";
import {
  WAttachmentDirectStatusSchema,
  type CoordWorkerDown,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  AttachmentDirectStatusResults,
  type AttachmentDirectStatusClock,
} from "../src/connect/attachment-direct-status-results.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";

const WORKER_FP = "a".repeat(64);
const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const UPLOAD_ID = "00000000-0000-4000-8000-000000000002";
let owner: AttachmentDirectStatusResults | null = null;
let worker: WorkerHandle;
let sent: CoordWorkerDown[];

function installWorker(epoch = "status-epoch"): WorkerHandle {
  sent = [];
  const handle: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: epoch,
    connectionGeneration: `status-${epoch}`,
    capabilities: new Set(),
    revoked: false,
    ready: true,
    send(frame): number {
      sent.push(frame);
      return 1;
    },
  };
  __setConnectWorkerForTest(WORKER_FP, handle);
  return handle;
}

function statusFrame(requestId: string, uploadId = UPLOAD_ID, committed = false) {
  return create(WAttachmentDirectStatusSchema, {
    requestId,
    status: create(AttachmentTransferStatusSchema, {
      uploadId,
      nextSeq: 2,
      bytesReceived: 1_024n,
      lastChunkSha256: "a".repeat(64),
      committed,
      absPath: committed ? "/attachment/path" : "",
      error: "",
    }),
  });
}

afterEach(() => {
  owner?.dispose();
  __setConnectWorkerForTest(WORKER_FP, null);
});

test("accepts only a current exact worker handle and matching upload receipt", async () => {
  owner = new AttachmentDirectStatusResults();
  worker = installWorker();
  const operation = owner.request(worker, SESSION_ID, UPLOAD_ID);
  operation.catch(() => {});
  const frame = sent.at(-1);
  if (frame?.frame.case !== "attachmentDirectStatusRequest") throw new Error("expected status request");
  const rogue: WorkerHandle = { ...worker, send: () => 1 };

  expect(owner.acceptStatus(rogue, statusFrame(frame.frame.value.requestId))).toBe(false);
  expect(owner.acceptStatus(worker, statusFrame(frame.frame.value.requestId, "other-upload"))).toBe(true);
  await expect(operation).rejects.toMatchObject({ code: Code.Unavailable });

  const followUp = owner.request(worker, SESSION_ID, UPLOAD_ID);
  followUp.catch(() => {});
  const followUpFrame = sent.at(-1);
  if (followUpFrame?.frame.case !== "attachmentDirectStatusRequest") throw new Error("expected follow-up status request");
  expect(owner.acceptStatus(worker, statusFrame(followUpFrame.frame.value.requestId, UPLOAD_ID, true))).toBe(true);
  await expect(followUp).resolves.toMatchObject({
    uploadId: UPLOAD_ID,
    nextSeq: 2,
    committed: true,
    absPath: "/attachment/path",
  });
});

test("does not accept a stale worker replacement or missing optional status", async () => {
  owner = new AttachmentDirectStatusResults();
  const first = installWorker();
  const original = owner.request(first, SESSION_ID, UPLOAD_ID);
  original.catch(() => {});
  const originalFrame = sent.at(-1);
  if (originalFrame?.frame.case !== "attachmentDirectStatusRequest") throw new Error("expected status request");
  const replacement = installWorker("status-replacement");

  expect(owner.acceptStatus(first, statusFrame(originalFrame.frame.value.requestId))).toBe(false);
  owner.cancelForWorkerHandle(first, "connection_superseded");
  await expect(original).rejects.toMatchObject({ code: Code.Unavailable });

  const current = owner.request(replacement, SESSION_ID, UPLOAD_ID);
  current.catch(() => {});
  const currentFrame = sent.at(-1);
  if (currentFrame?.frame.case !== "attachmentDirectStatusRequest") throw new Error("expected current status request");
  const missingStatus = create(WAttachmentDirectStatusSchema, { requestId: currentFrame.frame.value.requestId });
  expect(owner.acceptStatus(replacement, missingStatus)).toBe(true);
  await expect(current).rejects.toMatchObject({ code: Code.Unavailable });
});

test("times out a pending status request deterministically", async () => {
  const timers: Array<{ callback: () => void; unref(): void }> = [];
  const clock: AttachmentDirectStatusClock = {
    setTimeout(callback) {
      const timer = { callback, unref() {} };
      timers.push(timer);
      return timer as never;
    },
    clearTimeout() {},
  };
  owner = new AttachmentDirectStatusResults({ clock });
  worker = installWorker();
  const operation = owner.request(worker, SESSION_ID, UPLOAD_ID);
  operation.catch(() => {});
  timers[0]!.callback();

  await expect(operation).rejects.toMatchObject({ code: Code.DeadlineExceeded });
});
