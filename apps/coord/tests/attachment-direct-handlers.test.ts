// Focused attachment direct-control handler coverage. It proves the new grant
// RPC binds an authenticated tab and forwards an immutable descriptor only to
// the separate attachment grant owner. Router coverage keeps all attachment
// direct RPCs inside the coordinator's single service implementation.

import { expect, test } from "bun:test";
import { Code, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  AttachmentsGrantDirectRequestSchema,
  AttachmentsDirectStatusRequestSchema,
  CoordinatorService,
} from "@roost/protocol/proto/coordinator_pb";
import { ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY } from "@roost/protocol/attachment-transfer";
import { AttachmentTransferStatusSchema } from "@roost/protocol/proto/attachment_transfer_pb";
import { makeConnectBunHandler } from "../src/connect/bun-handler.ts";
import { makeAttachmentDirectHandlers } from "../src/connect/handlers-attachments-direct.ts";
import { callerKey, tabIdKey } from "../src/connect/auth-interceptor.ts";
import {
  type AttachmentGrantOwner,
  type AttachmentGrantRequest,
  type AttachmentGrantResult,
} from "../src/connect/attachment-grant-owner.ts";
import type { AttachmentDirectStatusResults } from "../src/connect/attachment-direct-status-results.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { buildConnectRouter } from "../src/connect/router.ts";
import type { WorkerHandle } from "../src/connect/worker-registry.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";

const DEVICE_FP = "d".repeat(64);
const WORKER_FP = "a".repeat(64);
const TAB_ID = "attachment-direct-tab";
const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const UPLOAD_ID = "00000000-0000-4000-8000-000000000002";

function browserContext(tabId = TAB_ID): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: DEVICE_FP,
    label: "Attachment browser",
    accountId: "test-account",
  });
  values.set(tabIdKey, tabId);
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

function grantRequest(tabId = TAB_ID) {
  return create(AttachmentsGrantDirectRequestSchema, {
    sessionId: SESSION_ID,
    workerFp: WORKER_FP,
    tabId,
    uploadId: UPLOAD_ID,
    filename: "diagram.png",
    shortPath: false,
    totalBytes: 1_024n,
  });
}
function attachmentSessionDb(workerFp: string) {
  type Query = {
    innerJoin(): Query;
    select(): Query;
    where(): Query;
    executeTakeFirst(): Promise<{ workerFp: string; status: string }>;
  };
  const query: Query = {
    innerJoin: () => query,
    select: () => query,
    where: () => query,
    executeTakeFirst: async () => ({ workerFp, status: "open" }),
  };
  return { selectFrom: () => query };
}

test("AttachmentsGrantDirect binds the authenticated tab and uses only separate attachment grant state", async () => {
  const capturedRequests: AttachmentGrantRequest[] = [];
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: "attachment-epoch",
    connectionGeneration: "attachment-handler-worker",
    capabilities: new Set([ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY]),
    revoked: false,
    ready: true,
    send: () => 1,
  };
  const granted: AttachmentGrantResult = {
    secret: "a".repeat(64),
    lease: {
      grantId: "attachment-grant",
      ownerKey: `account-device:test-account:${DEVICE_FP}`,
      deviceFingerprint: DEVICE_FP,
      tabId: TAB_ID,
      workerFp: WORKER_FP,
      workerEpoch: "attachment-epoch",
      descriptor: {
        sessionId: SESSION_ID,
        uploadId: UPLOAD_ID,
        filename: "diagram.png",
        shortPath: false,
        totalBytes: 1_024,
      },
      expiresAtMs: Date.now() + 60_000,
      workerHandle: worker,
    },
  };
  const attachmentGrants = {
    async grant(request: AttachmentGrantRequest): Promise<AttachmentGrantResult> {
      capturedRequests.push(request);
      await request.authorize();
      return granted;
    },
  } satisfies Pick<AttachmentGrantOwner, "grant">;
  const handlers = makeAttachmentDirectHandlers({
    cfg: { terminalPeerEnabled: true, terminalPeerStunUrls: ["stun:stun.example.test:3478"] },
    db: attachmentSessionDb(WORKER_FP),
    attachmentGrants,
  } as unknown as ConnectDeps);

  const response = await handlers.attachmentsGrantDirect(grantRequest(), browserContext());
  expect(response).toMatchObject({
    grantId: "attachment-grant",
    secret: "a".repeat(64),
    peerSupported: true,
    stunUrls: ["stun:stun.example.test:3478"],
  });
  const captured = capturedRequests.at(0);
  if (!captured) throw new Error("attachment grant request was not captured");
  expect(captured).toMatchObject({
    deviceFingerprint: DEVICE_FP,
    tabId: TAB_ID,
    workerFp: WORKER_FP,
    descriptor: {
      sessionId: SESSION_ID,
      uploadId: UPLOAD_ID,
      filename: "diagram.png",
      totalBytes: 1_024,
    },
  });

  await expect(handlers.attachmentsGrantDirect(grantRequest("other-tab"), browserContext()))
    .rejects.toMatchObject({ code: Code.PermissionDenied });
  expect(capturedRequests).toHaveLength(1);
  expect(captured.tabId).toBe(TAB_ID);
});

test("AttachmentsDirectStatus returns only an authenticated typed receipt", async () => {
  const worker: WorkerHandle = {
    workerFp: WORKER_FP,
    processEpoch: "attachment-epoch",
    connectionGeneration: "attachment-status-worker",
    capabilities: new Set(),
    revoked: false,
    ready: true,
    send: () => 1,
  };
  __setConnectWorkerForTest(WORKER_FP, worker);
  const observedRequests: Array<{
    worker: WorkerHandle;
    sessionId: string;
    uploadId: string;
  }> = [];
  const attachmentDirectStatusResults = {
    async request(
      ...[worker, sessionId, uploadId]: Parameters<AttachmentDirectStatusResults["request"]>
    ) {
      observedRequests.push({ worker, sessionId, uploadId });
      return create(AttachmentTransferStatusSchema, {
        uploadId,
        nextSeq: 2,
        bytesReceived: 1_024n,
        lastChunkSha256: "a".repeat(64),
        committed: true,
        absPath: "/attachment/path",
        error: "",
      });
    },
  } satisfies Pick<AttachmentDirectStatusResults, "request">;
  const handlers = makeAttachmentDirectHandlers({
    db: attachmentSessionDb(WORKER_FP),
    attachmentDirectStatusResults,
  } as unknown as ConnectDeps);
  try {
    const response = await handlers.attachmentsDirectStatus(create(AttachmentsDirectStatusRequestSchema, {
      sessionId: SESSION_ID,
      uploadId: UPLOAD_ID,
    }), browserContext());
    expect(observedRequests).toEqual([{
      worker,
      sessionId: SESSION_ID,
      uploadId: UPLOAD_ID,
    }]);
    expect(response.status).toMatchObject({ committed: true, absPath: "/attachment/path" });
  } finally {
    __setConnectWorkerForTest(WORKER_FP, null);
  }
});

test("the single coordinator router registers every attachment direct endpoint once", () => {
  const router = buildConnectRouter({ db: {}, cfg: {} } as unknown as ConnectDeps);
  const connectHandler = makeConnectBunHandler(router);
  for (const method of [
    CoordinatorService.method.attachmentsGrantDirect.name,
    CoordinatorService.method.attachmentsDirectStatus.name,
    CoordinatorService.method.sessionsNegotiateAttachmentPeer.name,
  ]) {
    const path = `/${CoordinatorService.typeName}/${method}`;
    expect(connectHandler.matches(path)).toBe(true);
    expect([...router.handlers].filter((handler) => handler.requestPath === path)).toHaveLength(1);
  }
});
