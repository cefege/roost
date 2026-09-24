// Defines the bounded pending record and admission vocabulary for terminal-peer
// signaling. TerminalPeerNegotiations owns the maps and lifecycle; this file
// holds no mutable registry and never retains SDP or other secret transport data.

import { Code, ConnectError } from "@connectrpc/connect";
import type {
  SessionsNegotiateLocalTerminalPeerRequest,
  SessionsNegotiateLocalTerminalPeerResponse,
} from "@roost/protocol/proto/coordinator_pb";
import type {
  WLocalTerminalPeerAnswer,
  WLocalTerminalPeerError,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  TERMINAL_PEER_MAX_SESSIONS_PER_GRANT,
  TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
} from "@roost/protocol/terminal-peer";
import { hasAtMostUtf8Bytes } from "@roost/protocol/ui-state";
import type { CoordConfig } from "@roost/host/config";
import type { KyselyDB } from "../../db/connection.ts";
import type {
  TerminalGrantInvalidation,
  TerminalGrantOwner,
} from "./terminal-grant-owner.ts";
import type { authorizeTerminalGrantSessions } from "./local-terminal-grants.ts";
import type { WorkerHandle } from "../../workers/worker-registry.ts";

export const TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES = 128;
const TERMINAL_PEER_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;

export type TerminalPeerNegotiationTimer = NodeJS.Timeout;
export type { TerminalGrantInvalidation };
export type TerminalPeerGrantPort = Pick<TerminalGrantOwner, "ownedGrant" | "subscribeInvalidation">;
export type TerminalGrantSessionAuthorizer = typeof authorizeTerminalGrantSessions;

export interface TerminalPeerNegotiationClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): TerminalPeerNegotiationTimer;
  clearTimeout(timer: TerminalPeerNegotiationTimer): void;
}

export interface TerminalPeerNegotiationsOptions {
  readonly db: KyselyDB;
  readonly cfg: Pick<CoordConfig, "terminalPeerEnabled" | "terminalPeerStunUrls">;
  readonly terminalGrants: TerminalPeerGrantPort;
  readonly currentWorker?: (workerFp: string) => WorkerHandle | null;
  readonly authorizeSessions?: TerminalGrantSessionAuthorizer;
  readonly clock?: TerminalPeerNegotiationClock;
  readonly createRequestId?: () => string;
  readonly answerTimeoutMs?: number;
}

export interface TerminalPeerNegotiationWorkerResultSink {
  acceptAnswer(source: WorkerHandle, answer: WLocalTerminalPeerAnswer): boolean;
  acceptError(source: WorkerHandle, error: WLocalTerminalPeerError): boolean;
  cancelForWorkerHandle(worker: WorkerHandle, reason: string): void;
}

export interface PendingTerminalPeerNegotiation {
  readonly requestId: string;
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly grantId: string;
  readonly peerId: string;
  readonly worker: WorkerHandle;
  readonly connectionGeneration: string;
  readonly workerEpoch: string;
  readonly offerDigest: string;
  readonly deadlineAtMono: number;
  readonly signal: AbortSignal;
  readonly promise: Promise<SessionsNegotiateLocalTerminalPeerResponse>;
  readonly resolve: (response: SessionsNegotiateLocalTerminalPeerResponse) => void;
  readonly reject: (error: Error) => void;
  abortListener: () => void;
  timer: TerminalPeerNegotiationTimer | null;
}

export const realTerminalPeerNegotiationClock: TerminalPeerNegotiationClock = {
  now: () => performance.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export function assertTerminalPeerRequestShape(
  request: SessionsNegotiateLocalTerminalPeerRequest,
): void {
  for (const [field, value] of [
    ["worker_fp", request.workerFp],
    ["grant_id", request.grantId],
    ["tab_id", request.tabId],
    ["peer_id", request.peerId],
    ["worker_epoch", request.workerEpoch],
  ] as const) {
    if (value.length === 0 || !hasAtMostUtf8Bytes(value, TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES)) {
      throw terminalPeerInvalid(`terminal peer ${field} is invalid`);
    }
  }
  if (!TERMINAL_PEER_UUID.test(request.peerId)) {
    throw terminalPeerInvalid("terminal peer peer_id is invalid");
  }
  if (!hasAtMostUtf8Bytes(request.offerSdp, TERMINAL_PEER_SDP_MAX_UTF8_BYTES)) {
    throw terminalPeerInvalid("terminal peer offer is invalid");
  }
}


export function hasValidTerminalPeerGrantSessions(sessionIds: readonly string[]): boolean {
  return (
    sessionIds.length > 0
    && sessionIds.length <= TERMINAL_PEER_MAX_SESSIONS_PER_GRANT
    && new Set(sessionIds).size === sessionIds.length
    && sessionIds.every((sessionId) => (
      sessionId.length > 0
      && hasAtMostUtf8Bytes(sessionId, TERMINAL_PEER_MAX_IDENTIFIER_UTF8_BYTES)
    ))
  );
}
export function terminalPeerKey(ownerKey: string, tabId: string, workerFp: string): string {
  return JSON.stringify([ownerKey, tabId, workerFp]);
}

export function terminalPeerInvalid(message: string): ConnectError {
  return new ConnectError(message, Code.InvalidArgument);
}

export function terminalPeerDenied(message: string): ConnectError {
  return new ConnectError(message, Code.PermissionDenied);
}

export function terminalPeerUnavailable(message: string): ConnectError {
  return new ConnectError(message, Code.Unavailable);
}

export function terminalPeerExhausted(message: string): ConnectError {
  return new ConnectError(message, Code.ResourceExhausted);
}

export function terminalPeerAlreadyExists(message: string): ConnectError {
  return new ConnectError(message, Code.AlreadyExists);
}

export function terminalPeerCancelled(message: string): ConnectError {
  return new ConnectError(message, Code.Canceled);
}

export function terminalPeerDeadlineExceeded(message: string): ConnectError {
  return new ConnectError(message, Code.DeadlineExceeded);
}

export function terminalPeerWorkerFailure(reason: string): Error {
  switch (reason) {
    case "grant_unavailable":
    case "expired":
      return terminalPeerDenied(`terminal peer worker rejected negotiation: ${reason}`);
    case "capacity":
      return terminalPeerExhausted(`terminal peer worker rejected negotiation: ${reason}`);
    default:
      return terminalPeerUnavailable(`terminal peer worker rejected negotiation: ${reason}`);
  }
}

export function incrementTerminalPeerCount(counts: Map<string, number>, key: string): void {
  counts.set(key, (counts.get(key) ?? 0) + 1);
}

export function decrementTerminalPeerCount(counts: Map<string, number>, key: string): void {
  const count = counts.get(key);
  if (count === undefined || count <= 1) counts.delete(key);
  else counts.set(key, count - 1);
}
