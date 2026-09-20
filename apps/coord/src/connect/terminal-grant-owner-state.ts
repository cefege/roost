// State shapes, bounded failure vocabulary, and exact-worker helpers for grants.
// TerminalGrantOwner keeps mutation and lifecycle ordering in its sibling.
// Type-only imports preserve one public contract without another lease registry.

import { Code, ConnectError } from "@connectrpc/connect";
import type {
  TerminalDirectRetireReason,
  TerminalGrantAuthorization,
  TerminalGrantLeaseSnapshot,
  TerminalGrantResult,
} from "./terminal-grant-owner.ts";
import type { WorkerHandle } from "./worker-registry.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

export type TerminalGrantInvalidationKind = "grant_expired" | "device_revoked"
  | "grant_replaced" | "scope_reduced" | "worker_retired" | "disposed";

export interface TerminalGrantInvalidation {
  readonly kind: TerminalGrantInvalidationKind;
  readonly lease: TerminalGrantLeaseSnapshot | null;
  readonly workerFp: string;
  readonly workerEpoch: string | null;
  readonly deviceFingerprint: string | null;
  readonly removedSessionIds: readonly string[];
  readonly reason: TerminalDirectRetireReason | null;
}

export interface TerminalGrantLease extends TerminalGrantLeaseSnapshot {
  timer: NodeJS.Timeout | undefined;
}

export interface PendingGrantRefresh {
  readonly key: string;
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly sessionIds: Set<string>;
  authorize: TerminalGrantAuthorization;
  invalidated: boolean;
  readonly promise: Promise<TerminalGrantResult>;
  readonly resolve: (result: TerminalGrantResult) => void;
  readonly reject: (error: unknown) => void;
}

export function terminalGrantLeaseKey(ownerKey: string, tabId: string, workerFp: string): string {
  return JSON.stringify([ownerKey, tabId, workerFp]);
}

export function terminalGrantSnapshot(
  lease: TerminalGrantLeaseSnapshot,
): TerminalGrantLeaseSnapshot {
  return {
    grantId: lease.grantId,
    ownerKey: lease.ownerKey,
    deviceFingerprint: lease.deviceFingerprint,
    tabId: lease.tabId,
    workerFp: lease.workerFp,
    workerEpoch: lease.workerEpoch,
    sessionIds: lease.sessionIds,
    expiresAtMs: lease.expiresAtMs,
    workerHandle: lease.workerHandle,
  };
}

export function isExactTerminalGrantWorker(
  worker: WorkerHandle,
  workerEpoch: string | null,
): boolean {
  return worker.processEpoch === workerEpoch
    && currentRoutableWorker(worker.workerFp) === worker;
}

export function invalidGrantSessions(): ConnectError {
  return new ConnectError("terminal grant sessions are invalid", Code.InvalidArgument);
}

export function grantCapacityExceeded(): ConnectError {
  return new ConnectError("terminal grant refresh capacity is exhausted", Code.ResourceExhausted);
}

export function workerUnavailable(): ConnectError {
  return new ConnectError("worker unavailable", Code.Unavailable);
}
