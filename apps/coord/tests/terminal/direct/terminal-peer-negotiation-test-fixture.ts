// Shared fixtures for focused coordinator terminal-peer signaling tests. They
// use typed worker frames and exact registry handles without a real WebSocket
// or database, while the production owner still invokes its canonical authorizer.
// SDP is fixture input only and never captured by diagnostics or assertions.

import { create } from "@bufbuild/protobuf";
import type { AccountDeviceCaller } from "../../../src/auth/auth-interceptor.ts";
import { captureOwnerKey } from "../../../src/terminal/capture/terminal-capture-lease.ts";
import {
  TerminalPeerNegotiations,
} from "../../../src/terminal/direct/terminal-peer-negotiations.ts";
import type {
  TerminalGrantInvalidation,
  TerminalGrantLeaseSnapshot,
} from "../../../src/terminal/direct/terminal-grant-owner.ts";
import {
  __setConnectWorkerForTest,
  type WorkerHandle,
} from "../../../src/workers/worker-registry.ts";
import type {
  CoordWorkerDown,
  DLocalTerminalPeerOffer,
  WLocalTerminalPeerAnswer,
} from "@roost/protocol/proto/worker_transport_pb";
import { WLocalTerminalPeerAnswerSchema } from "@roost/protocol/proto/worker_transport_pb";
import {
  SessionsNegotiateLocalTerminalPeerRequestSchema,
  type SessionsNegotiateLocalTerminalPeerRequest,
} from "@roost/protocol/proto/coordinator_pb";
import { TERMINAL_PEER_WEBRTC_CAPABILITY } from "@roost/protocol/terminal-peer";
import type {
  TerminalPeerGrantPort,
  TerminalPeerNegotiationClock,
  TerminalPeerNegotiationTimer,
} from "../../../src/terminal/direct/terminal-peer-negotiation-state.ts";

export const TEST_CALLER: AccountDeviceCaller = {
  kind: "account-device",
  fingerprint: "d".repeat(64),
  label: "Test browser",
  accountId: "test-account",
};
export const TEST_OWNER_KEY = captureOwnerKey(TEST_CALLER);
export const TEST_TAB_ID = "terminal-peer-test-tab";
export const TEST_GRANT_ID = "terminal-peer-test-grant";
export const TEST_WORKER_FP = "a".repeat(64);
export const TEST_WORKER_EPOCH = "terminal-peer-test-epoch";
export const VALID_TERMINAL_PEER_SDP = [
  "v=0",
  "o=- 1 2 IN IP4 127.0.0.1",
  "s=-",
  "t=0 0",
  "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
  "a=setup:actpass",
  `a=fingerprint:sha-256 ${Array.from({ length: 32 }, (_, index) => index.toString(16).padStart(2, "0")).join(":")}`,
  "a=ice-ufrag:peer-offer",
  `a=ice-pwd:${"p".repeat(22)}`,
  "a=max-message-size:16384",
  "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host",
  "",
].join("\r\n");

const inertTimer = { unref() {} } as unknown as TerminalPeerNegotiationTimer;
const inertClock: TerminalPeerNegotiationClock = {
  now: () => 0,
  setTimeout: () => inertTimer,
  clearTimeout() {},
};

export class TestTerminalGrants implements TerminalPeerGrantPort {
  private readonly leases = new Map<string, TerminalGrantLeaseSnapshot>();
  private readonly listeners = new Set<(event: TerminalGrantInvalidation) => void>();

  ownedGrant(ownerKey: string, tabId: string, workerFp: string, grantId: string): TerminalGrantLeaseSnapshot | null {
    const lease = this.leases.get(JSON.stringify([ownerKey, tabId, workerFp]));
    return lease?.grantId === grantId ? lease : null;
  }

  subscribeInvalidation(listener: (event: TerminalGrantInvalidation) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  install(lease: TerminalGrantLeaseSnapshot): void {
    this.leases.set(JSON.stringify([lease.ownerKey, lease.tabId, lease.workerFp]), lease);
  }

  invalidate(event: TerminalGrantInvalidation): void {
    for (const listener of this.listeners) listener(event);
  }
}
let workerConnectionSequence = 0;


export interface TestWorker {
  readonly worker: WorkerHandle;
  readonly sent: CoordWorkerDown[];
}

export function installTestWorker(
  workerFp = TEST_WORKER_FP,
  workerEpoch = TEST_WORKER_EPOCH,
  capabilities: readonly string[] = [TERMINAL_PEER_WEBRTC_CAPABILITY],
): TestWorker {
  const sent: CoordWorkerDown[] = [];
  const worker: WorkerHandle = {
    workerFp,
    processEpoch: workerEpoch,
    connectionGeneration: `connection-${workerFp.slice(0, 8)}-${workerEpoch}-${++workerConnectionSequence}`,
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

export function installLease(
  grants: TestTerminalGrants,
  worker: WorkerHandle,
  options: Partial<Pick<TerminalGrantLeaseSnapshot, "ownerKey" | "deviceFingerprint" | "tabId" | "grantId" | "sessionIds">> = {},
): TerminalGrantLeaseSnapshot {
  const lease: TerminalGrantLeaseSnapshot = {
    grantId: options.grantId ?? TEST_GRANT_ID,
    ownerKey: options.ownerKey ?? TEST_OWNER_KEY,
    deviceFingerprint: options.deviceFingerprint ?? TEST_CALLER.fingerprint,
    tabId: options.tabId ?? TEST_TAB_ID,
    workerFp: worker.workerFp,
    workerEpoch: worker.processEpoch,
    sessionIds: options.sessionIds ?? ["00000000-0000-4000-8000-000000000001"],
    expiresAtMs: Date.now() + 60_000,
    workerHandle: worker,
  };
  grants.install(lease);
  return lease;
}

export function createPeerNegotiations(
  grants: TestTerminalGrants,
  options: {
    readonly clock?: TerminalPeerNegotiationClock;
    readonly answerTimeoutMs?: number;
    readonly authorizeSessions?: () => Promise<void>;
    readonly peerEnabled?: boolean;
  } = {},
): TerminalPeerNegotiations {
  return new TerminalPeerNegotiations({
    db: {} as never,
    cfg: {
      terminalPeerEnabled: options.peerEnabled ?? true,
      terminalPeerStunUrls: [],
    },
    terminalGrants: grants,
    clock: options.clock ?? inertClock,
    answerTimeoutMs: options.answerTimeoutMs,
    authorizeSessions: async () => {
      await options.authorizeSessions?.();
    },
  });
}

export function peerRequest(
  worker: WorkerHandle,
  options: Partial<Pick<SessionsNegotiateLocalTerminalPeerRequest, "grantId" | "tabId" | "peerId" | "offerSdp" | "workerEpoch">> = {},
): SessionsNegotiateLocalTerminalPeerRequest {
  return create(SessionsNegotiateLocalTerminalPeerRequestSchema, {
    workerFp: worker.workerFp,
    grantId: options.grantId ?? TEST_GRANT_ID,
    tabId: options.tabId ?? TEST_TAB_ID,
    peerId: options.peerId ?? "00000000-0000-4000-8000-000000000010",
    offerSdp: options.offerSdp ?? VALID_TERMINAL_PEER_SDP,
    workerEpoch: options.workerEpoch ?? worker.processEpoch ?? "",
  });
}

export function offerFrom(sent: readonly CoordWorkerDown[]): DLocalTerminalPeerOffer {
  const frame = sent.at(-1);
  if (frame?.frame.case !== "localTerminalPeerOffer") throw new Error("expected terminal peer offer");
  return frame.frame.value;
}

export async function beginPeerNegotiation(
  owner: TerminalPeerNegotiations,
  testWorker: TestWorker,
  signal = new AbortController().signal,
  request = peerRequest(testWorker.worker),
) {
  const operation = owner.negotiate(TEST_CALLER, TEST_TAB_ID, request, signal);
  operation.catch(() => {});
  for (let attempt = 0; attempt < 4; attempt += 1) await Promise.resolve();
  return { operation, request, offer: offerFrom(testWorker.sent) };
}

export function peerAnswer(
  offer: DLocalTerminalPeerOffer,
  worker: WorkerHandle,
): WLocalTerminalPeerAnswer {
  return create(WLocalTerminalPeerAnswerSchema, {
    requestId: offer.requestId,
    connectionGeneration: worker.connectionGeneration,
    workerEpoch: worker.processEpoch ?? "",
    peerId: offer.peerId,
    answerSdp: VALID_TERMINAL_PEER_SDP,
  });
}
