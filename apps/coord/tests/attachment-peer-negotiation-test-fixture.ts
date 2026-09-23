// Shared fixtures for focused coordinator attachment-peer signaling tests. They
// use typed worker frames and exact in-memory attachment grants without a socket
// or database. SDP is fixture input only and never appears in logs or assertions.
// The production owner verifies the separately minted immutable grant tuple.

import { create } from "@bufbuild/protobuf";
import type { AccountDeviceCaller } from "../src/connect/auth-interceptor.ts";
import { captureOwnerKey } from "../src/connect/terminal-capture-lease.ts";
import { AttachmentPeerNegotiations } from "../src/connect/attachment-peer-negotiations.ts";
import type {
  AttachmentGrantDescriptor,
  AttachmentGrantInvalidation,
  AttachmentGrantLeaseSnapshot,
  AttachmentGrantPort,
} from "../src/connect/attachment-grant-owner-state.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../src/connect/worker-registry.ts";
import type {
  CoordWorkerDown,
  DLocalAttachmentPeerOffer,
  WLocalAttachmentPeerAnswer,
} from "@roost/shared/proto/worker_transport_pb";
import { WLocalAttachmentPeerAnswerSchema } from "@roost/shared/proto/worker_transport_pb";
import {
  SessionsNegotiateAttachmentPeerRequestSchema,
  type SessionsNegotiateAttachmentPeerRequest,
} from "@roost/shared/proto/coordinator_pb";
import { ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY } from "@roost/shared/attachment-transfer";
import type {
  AttachmentPeerNegotiationClock,
  AttachmentPeerNegotiationTimer,
} from "../src/connect/attachment-peer-negotiation-state.ts";

export const TEST_ATTACHMENT_CALLER: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "d".repeat(64),
  label: "Attachment browser",
  accountId: "test-account",
};
export const TEST_ATTACHMENT_OWNER_KEY = captureOwnerKey(TEST_ATTACHMENT_CALLER);
export const TEST_ATTACHMENT_TAB_ID = "attachment-peer-test-tab";
export const TEST_ATTACHMENT_GRANT_ID = "attachment-peer-test-grant";
export const TEST_ATTACHMENT_WORKER_FP = "a".repeat(64);
export const TEST_ATTACHMENT_WORKER_EPOCH = "attachment-peer-test-epoch";
export const VALID_ATTACHMENT_PEER_SDP = [
  "v=0",
  "o=- 1 2 IN IP4 127.0.0.1",
  "s=-",
  "t=0 0",
  "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
  "a=setup:actpass",
  `a=fingerprint:sha-256 ${Array.from({ length: 32 }, (_, index) => index.toString(16).padStart(2, "0")).join(":")}`,
  "a=ice-ufrag:attachment-offer",
  `a=ice-pwd:${"p".repeat(22)}`,
  "a=max-message-size:16384",
  "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host",
  "",
].join("\r\n");

const inertTimer = { unref() {} } as unknown as AttachmentPeerNegotiationTimer;
const inertClock: AttachmentPeerNegotiationClock = {
  now: () => 0,
  setTimeout: () => inertTimer,
  clearTimeout() {},
};

export class TestAttachmentGrants implements AttachmentGrantPort {
  private readonly leases = new Map<string, AttachmentGrantLeaseSnapshot>();
  private readonly listeners = new Set<(event: AttachmentGrantInvalidation) => void>();

  ownedGrant(ownerKey: string, tabId: string, workerFp: string, grantId: string): AttachmentGrantLeaseSnapshot | null {
    const lease = this.leases.get(grantId);
    if (!lease || lease.ownerKey !== ownerKey || lease.tabId !== tabId || lease.workerFp !== workerFp) return null;
    return lease;
  }

  subscribeInvalidation(listener: (event: AttachmentGrantInvalidation) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  install(lease: AttachmentGrantLeaseSnapshot): void {
    this.leases.set(lease.grantId, lease);
  }

  invalidate(event: AttachmentGrantInvalidation): void {
    for (const listener of this.listeners) listener(event);
  }
}

let workerConnectionSequence = 0;

export interface TestAttachmentWorker {
  readonly worker: WorkerHandle;
  readonly sent: CoordWorkerDown[];
}

export function installAttachmentTestWorker(
  workerFp = TEST_ATTACHMENT_WORKER_FP,
  workerEpoch = TEST_ATTACHMENT_WORKER_EPOCH,
  capabilities: readonly string[] = [ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY],
): TestAttachmentWorker {
  const sent: CoordWorkerDown[] = [];
  const worker: WorkerHandle = {
    workerFp,
    processEpoch: workerEpoch,
    connectionGeneration: `attachment-connection-${workerFp.slice(0, 8)}-${workerEpoch}-${++workerConnectionSequence}`,
    capabilities: new Set(capabilities),
    revoked: false,
    ready: true,
    send(frame): number {
      sent.push(frame);
      return 1;
    },
  };
  __setConnectWorkerForTest(workerFp, worker);
  return { worker, sent };
}

export function installAttachmentLease(
  grants: TestAttachmentGrants,
  worker: WorkerHandle,
  options: {
    readonly ownerKey?: string;
    readonly deviceFingerprint?: string;
    readonly tabId?: string;
    readonly grantId?: string;
    readonly descriptor?: Partial<AttachmentGrantDescriptor>;
  } = {},
): AttachmentGrantLeaseSnapshot {
  const descriptor: AttachmentGrantDescriptor = {
    sessionId: options.descriptor?.sessionId ?? "00000000-0000-4000-8000-000000000001",
    uploadId: options.descriptor?.uploadId ?? "00000000-0000-4000-8000-000000000002",
    filename: options.descriptor?.filename ?? "attachment.bin",
    shortPath: options.descriptor?.shortPath ?? false,
    totalBytes: options.descriptor?.totalBytes ?? 1_024,
  };
  const lease: AttachmentGrantLeaseSnapshot = {
    grantId: options.grantId ?? TEST_ATTACHMENT_GRANT_ID,
    ownerKey: options.ownerKey ?? TEST_ATTACHMENT_OWNER_KEY,
    deviceFingerprint: options.deviceFingerprint ?? TEST_ATTACHMENT_CALLER.fingerprint,
    tabId: options.tabId ?? TEST_ATTACHMENT_TAB_ID,
    workerFp: worker.workerFp,
    workerEpoch: worker.processEpoch ?? "",
    descriptor,
    expiresAtMs: Date.now() + 60_000,
    workerHandle: worker,
  };
  grants.install(lease);
  return lease;
}

export function createAttachmentPeerNegotiations(
  grants: TestAttachmentGrants,
  options: {
    readonly clock?: AttachmentPeerNegotiationClock;
    readonly answerTimeoutMs?: number;
    readonly peerEnabled?: boolean;
  } = {},
): AttachmentPeerNegotiations {
  return new AttachmentPeerNegotiations({
    cfg: {
      terminalPeerEnabled: options.peerEnabled ?? true,
      terminalPeerStunUrls: [],
    },
    attachmentGrants: grants,
    clock: options.clock ?? inertClock,
    answerTimeoutMs: options.answerTimeoutMs,
  });
}

export function attachmentPeerRequest(
  worker: WorkerHandle,
  options: Partial<Pick<SessionsNegotiateAttachmentPeerRequest, "grantId" | "tabId" | "peerId" | "offerSdp" | "workerEpoch">> = {},
): SessionsNegotiateAttachmentPeerRequest {
  return create(SessionsNegotiateAttachmentPeerRequestSchema, {
    workerFp: worker.workerFp,
    grantId: options.grantId ?? TEST_ATTACHMENT_GRANT_ID,
    tabId: options.tabId ?? TEST_ATTACHMENT_TAB_ID,
    peerId: options.peerId ?? "00000000-0000-4000-8000-000000000010",
    offerSdp: options.offerSdp ?? VALID_ATTACHMENT_PEER_SDP,
    workerEpoch: options.workerEpoch ?? worker.processEpoch ?? "",
  });
}

export function attachmentOfferFrom(sent: readonly CoordWorkerDown[]): DLocalAttachmentPeerOffer {
  const frame = sent.at(-1);
  if (frame?.frame.case !== "localAttachmentPeerOffer") throw new Error("expected attachment peer offer");
  return frame.frame.value;
}

export async function beginAttachmentPeerNegotiation(
  owner: AttachmentPeerNegotiations,
  testWorker: TestAttachmentWorker,
  signal = new AbortController().signal,
  request = attachmentPeerRequest(testWorker.worker),
) {
  const operation = owner.negotiate(TEST_ATTACHMENT_CALLER, TEST_ATTACHMENT_TAB_ID, request, signal);
  operation.catch(() => {});
  for (let attempt = 0; attempt < 4; attempt += 1) await Promise.resolve();
  return { operation, request, offer: attachmentOfferFrom(testWorker.sent) };
}

export function attachmentPeerAnswer(
  offer: DLocalAttachmentPeerOffer,
  worker: WorkerHandle,
): WLocalAttachmentPeerAnswer {
  return create(WLocalAttachmentPeerAnswerSchema, {
    requestId: offer.requestId,
    connectionGeneration: worker.connectionGeneration,
    workerEpoch: worker.processEpoch ?? "",
    peerId: offer.peerId,
    answerSdp: VALID_ATTACHMENT_PEER_SDP,
  });
}
