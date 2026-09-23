// Approver pairing lifecycle for PairApprovalProvider. Gathers PairApprovalStatus
// and PairDeny results as evidence, decides which failures retry without
// discarding the generated code, and maps evidence to the provider's next step.
// The provider owns every timer, fence, and signal. Depends on connect.ts for
// the coordinator client and its device-rejection classifier.

import { Code, ConnectError } from "@connectrpc/connect";
import { PAIRING_CEREMONY_VERSION } from "@roost/shared/pairing";
import { backoffDelayMs } from "@roost/shared/retry";
import { classifyAuthFailure, coordClient } from "../connect.ts";
import type { ToastKind } from "../store/toastStore.ts";
import { isTransientPairingError } from "./pairing-transient-error.ts";

export const PAIR_APPROVE_PATH = "/roost.v1.CoordinatorService/PairApprove";
export const PAIR_DENY_PATH = "/roost.v1.CoordinatorService/PairDeny";
export const PAIR_APPROVAL_STATUS_PATH = "/roost.v1.CoordinatorService/PairApprovalStatus";

const RETRY_BASE_MS = 1_000;
const RETRY_MAX_MS = 30_000;

export type PairApprovalStatus =
  | "verification_required"
  | "completed"
  | "denied"
  | "expired"
  | "verification_failed";

export type PairApprovalOutcome =
  | Exclude<PairApprovalStatus, "verification_required">
  | "cancelled"
  | "unavailable"
  | "authority"
  | "reload";

export type ApprovalStatusErrorClass = "retry" | "unavailable" | "authority" | "reload";

export type ApprovalStatusEvidence =
  | { kind: "status_read"; status: string }
  | { kind: "status_failed"; error: unknown };

export type CancellationEvidence =
  | ApprovalStatusEvidence
  | { kind: "deny_acknowledged" }
  | { kind: "deny_failed"; error: unknown };

/** What the provider does next with the operation that produced the evidence. */
export type PairApprovalStep =
  | { kind: "poll" }
  | { kind: "retry"; error: unknown }
  | { kind: "read_status" }
  | { kind: "send_deny" }
  | { kind: "settle"; outcome: PairApprovalOutcome };

export type RetiringOutcome = Exclude<PairApprovalOutcome, "completed" | "reload">;

export const PAIR_APPROVAL_OUTCOME_TOASTS: Record<RetiringOutcome, { message: string; kind: ToastKind }> = {
  cancelled: { message: "Pairing request cancelled.", kind: "ok" },
  denied: { message: "Pairing request was denied.", kind: "warn" },
  expired: { message: "Pair request expired.", kind: "warn" },
  verification_failed: { message: "Pairing verification failed.", kind: "warn" },
  unavailable: { message: "Pairing request is no longer available.", kind: "warn" },
  authority: { message: "Pairing authority is no longer valid.", kind: "err" },
};

/** Transport failures, transient server codes, and an unmarked (front-door)
 *  401 retry. A 401 counts as a device rejection only when connect.ts
 *  classifies it as one for `rpcPath`. */
export function isRetryablePairApprovalError(error: unknown, rpcPath: string): boolean {
  if (error instanceof ConnectError && error.code === Code.Unauthenticated) {
    return classifyAuthFailure(error, rpcPath) !== "device";
  }
  return isTransientPairingError(error);
}

export function classifyApprovalStatusError(error: unknown): ApprovalStatusErrorClass {
  return classifyPairRpcFailure(error, PAIR_APPROVAL_STATUS_PATH);
}

export function parseApprovalStatus(status: string): PairApprovalStatus | "unknown" {
  switch (status) {
    case "verification_required":
    case "completed":
    case "denied":
    case "expired":
    case "verification_failed":
      return status;
    default:
      return "unknown";
  }
}

/** Status evidence gathered while the code dialog waits for the requester. */
export function resolveApprovalStatus(evidence: ApprovalStatusEvidence): PairApprovalStep {
  if (evidence.kind === "status_failed") {
    const failure = classifyApprovalStatusError(evidence.error);
    return failure === "retry" ? { kind: "retry", error: evidence.error } : settle(failure);
  }
  const status = parseApprovalStatus(evidence.status);
  if (status === "verification_required") return { kind: "poll" };
  return settle(status === "unknown" ? "reload" : status);
}

/** PairDeny and follow-up status evidence gathered after the approver cancels. */
export function resolveCancellation(evidence: CancellationEvidence): PairApprovalStep {
  switch (evidence.kind) {
    case "deny_acknowledged":
      return settle("cancelled");
    case "deny_failed": {
      const failure = classifyPairRpcFailure(evidence.error, PAIR_DENY_PATH);
      if (failure === "retry") return { kind: "retry", error: evidence.error };
      // NotFound: the row already left the live states, possibly through this
      // very denial whose response was lost, so the status read decides.
      return failure === "unavailable" ? { kind: "read_status" } : settle(failure);
    }
    case "status_failed":
      return resolveApprovalStatus(evidence);
    case "status_read": {
      const status = parseApprovalStatus(evidence.status);
      if (status === "verification_required") return { kind: "send_deny" };
      if (status === "denied") return settle("cancelled");
      return settle(status === "unknown" ? "reload" : status);
    }
  }
}

/** Never rejects: a failure becomes evidence for resolveApprovalStatus or
 *  resolveCancellation to classify. */
export async function requestApprovalStatus(ephemeralId: string): Promise<ApprovalStatusEvidence> {
  try {
    const response = await coordClient.pairApprovalStatus({
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
      ephemeralId,
    });
    return { kind: "status_read", status: response.status };
  } catch (error) {
    return { kind: "status_failed", error };
  }
}

export async function requestPairDenial(ephemeralId: string): Promise<CancellationEvidence> {
  try {
    await coordClient.pairDeny({ ephemeralId });
    return { kind: "deny_acknowledged" };
  } catch (error) {
    return { kind: "deny_failed", error };
  }
}

export function pairApprovalRetryDelayMs(attempt: number, error: unknown): number {
  const fallback = backoffDelayMs(attempt, { baseMs: RETRY_BASE_MS, maxMs: RETRY_MAX_MS });
  if (!(error instanceof ConnectError)) return fallback;
  const retryAfterSeconds = Number(error.metadata.get("retry-after"));
  if (!Number.isFinite(retryAfterSeconds) || retryAfterSeconds <= 0) return fallback;
  return Math.min(RETRY_MAX_MS, Math.ceil(retryAfterSeconds * 1_000));
}

function settle(outcome: PairApprovalOutcome): PairApprovalStep {
  return { kind: "settle", outcome };
}

function classifyPairRpcFailure(error: unknown, rpcPath: string): ApprovalStatusErrorClass {
  if (!(error instanceof ConnectError) || isRetryablePairApprovalError(error, rpcPath)) return "retry";
  switch (error.code) {
    case Code.NotFound:
      return "unavailable";
    // Unauthenticated reaches here only as a classified device rejection.
    case Code.Unauthenticated:
    case Code.PermissionDenied:
      return "authority";
    // A stale ceremony version, Unimplemented (an older coordinator), and any
    // other unclassified refusal keep the code: only a reload can progress.
    default:
      return "reload";
  }
}
