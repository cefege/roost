// Terminal input control: one logical PTY input batch per bounded sender lane,
// with the audit-log queue that persists its outcome. Terminal view membership
// and SCD are intentionally absent; input admission remains session-scoped.

import { signal } from "@roost/shared/diag";
import {
  TerminalInputStatus,
  TerminalWritePhase,
  type WInputResult,
} from "@roost/shared/proto/worker_transport_pb";
import type { ConnectDeps } from "./router.ts";
import {
  sendTerminalInputRequest,
  startHopDeadline,
  INPUT_CONTROL_TIMEOUT_MS,
  type HopDeadline,
} from "./worker-send.ts";
import {
  enqueueLane,
  resolveSessionRoute,
  type TerminalControlGeneration,
  type TerminalViewerIdentity,
} from "./terminal-control-lane.ts";
import { writeAuditLog } from "../middleware/security.ts";
import type { WriteLease } from "../coord-move/write-gate.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const INPUT_AUDIT_QUEUE_CAP = 1_024;

export type InputControlResult =
  | {
      status: "accepted";
      sessionId: string;
      inputSeq: bigint;
      writtenBytes: number;
    }
  | {
      status: "rejected";
      sessionId: string;
      inputSeq: bigint;
      writtenBytes: 0;
      reason: string;
    }
  | {
      status: "ambiguous";
      sessionId: string;
      inputSeq: bigint;
      writtenBytes: number;
      reason: string;
    };

export interface InputControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  inputSeq: bigint;
  data: Uint8Array;
  socketGeneration?: TerminalControlGeneration;
  audit?: { traceId?: string };
  /** Monotonic hop budget shared by the lane wait and the worker send.
   * Injected by tests; production starts it at entry. */
  deadline?: HopDeadline;
}

const inputRejected = (
  command: Pick<InputControlCommand, "sessionId" | "inputSeq">,
  reason: string,
): InputControlResult => ({
  status: "rejected",
  sessionId: command.sessionId,
  inputSeq: command.inputSeq,
  writtenBytes: 0,
  reason: reason.slice(0, 200),
});

interface InputAuditRecord {
  deps: ConnectDeps;
  callerFingerprint: string;
  outcome: InputControlResult["status"];
  writtenBytes: number;
  traceId?: string;
}

interface QueuedInputAudit extends InputAuditRecord {
  resolve: () => void;
  reject: (error: unknown) => void;
}

const inputAuditQueue: QueuedInputAudit[] = [];
const inputAuditCapacityWaiters: Array<() => void> = [];
let inputAuditPumping = false;

function pumpInputAudits(): void {
  if (inputAuditPumping) return;
  inputAuditPumping = true;
  queueMicrotask(async () => {
    try {
      while (inputAuditQueue.length > 0) {
        const next = inputAuditQueue.shift()!;
        inputAuditCapacityWaiters.shift()?.();
        try {
          await writeAuditLog({
            db: next.deps.db,
            status: next.outcome === "accepted" ? 200 : next.outcome === "ambiguous" ? 409 : 422,
            method: "SYNC",
            path: `/ws/coord-sync/input/${next.outcome}/${next.writtenBytes}/SessionsInput`,
            traceId: next.traceId,
            callerFp: next.callerFingerprint,
            throwOnFailure: true,
          });
          next.resolve();
        } catch (error) {
          next.reject(error);
        }
      }
    } finally {
      inputAuditPumping = false;
      if (inputAuditQueue.length > 0) pumpInputAudits();
    }
  });
}

async function enqueueInputAudit(entry: InputAuditRecord): Promise<void> {
  if (inputAuditQueue.length >= INPUT_AUDIT_QUEUE_CAP) {
    signal("audit.input_queue_backpressure", {
      caller_fp: entry.callerFingerprint,
      cooldownKey: "terminal-input",
    });
    await new Promise<void>((resolve) => inputAuditCapacityWaiters.push(resolve));
  }
  return new Promise<void>((resolve, reject) => {
    inputAuditQueue.push({ ...entry, resolve, reject });
    pumpInputAudits();
  });
}

function classifyWorkerInput(
  command: InputControlCommand,
  result: WInputResult,
): InputControlResult {
  const written = Number.isSafeInteger(result.writtenBytes)
    ? Math.max(0, Math.min(result.writtenBytes, command.data.byteLength))
    : 0;
  if (result.status === TerminalInputStatus.ACCEPTED && written === command.data.byteLength) {
    return {
      status: "accepted",
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      writtenBytes: written,
    };
  }
  // A rejection is only definite when the worker proves it stopped before the
  // keeper write; otherwise the batch may already be on the PTY and calling it
  // unsent would invite a duplicate.
  if (
    result.status === TerminalInputStatus.REJECTED
    && result.phase === TerminalWritePhase.PRE_WRITE
    && result.writtenBytes === 0
  ) {
    return inputRejected(command, result.reason || "keeper rejected input");
  }
  return {
    status: "ambiguous",
    sessionId: command.sessionId,
    inputSeq: command.inputSeq,
    writtenBytes: written,
    reason: (result.reason || "input completion could not be proven").slice(0, 200),
  };
}

/** Route one logical input batch exactly once. Once the worker transport admits
 * the request, any missing/malformed result is ambiguous and is never retried. */
export function processInputControl(
  deps: ConnectDeps,
  command: InputControlCommand,
): Promise<InputControlResult> {
  const finish = (result: Promise<InputControlResult>): Promise<InputControlResult> => {
    if (!command.audit) return result;
    return result.then(async (outcome) => {
      try {
        await enqueueInputAudit({
          deps,
          callerFingerprint: command.identity.callerFingerprint,
          outcome: outcome.status,
          writtenBytes: outcome.writtenBytes,
          traceId: command.audit?.traceId,
        });
        return outcome;
      } catch (error) {
        const auditReason = `input audit persistence failed: ${
          error instanceof Error ? error.message : String(error)
        }`.slice(0, 200);
        if (outcome.status === "rejected") {
          return { ...outcome, reason: auditReason };
        }
        return {
          status: "ambiguous",
          sessionId: outcome.sessionId,
          inputSeq: outcome.inputSeq,
          writtenBytes: outcome.writtenBytes,
          reason: auditReason,
        };
      }
    });
  };
  if (command.data.byteLength === 0) {
    return finish(Promise.resolve({
      status: "accepted",
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      writtenBytes: 0,
    }));
  }
  if (command.data.byteLength > MAX_INPUT_BYTES) {
    return finish(Promise.resolve(inputRejected(command, "input exceeds 64 KiB")));
  }
  const socketGeneration = command.socketGeneration ?? 0;
  // The budget starts before the lane wait so queueing cannot mint a fresh
  // deadline at worker admission.
  const deadline = command.deadline ?? startHopDeadline(INPUT_CONTROL_TIMEOUT_MS);
  return finish(enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async (releaseLane) => {
      let lease: WriteLease | null = null;
      let admitted = false;
      try {
        lease = deps.move?.gate.acquire() ?? null;
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return inputRejected(command, "unknown session");
        const workerCall = sendTerminalInputRequest(route.workerFp, {
          sessionId: command.sessionId,
          inputSeq: command.inputSeq,
          data: command.data,
        }, deadline);
        admitted = workerCall.admitted;
        if (!admitted) {
          void workerCall.result.catch(() => undefined);
          // Nothing reached the socket, so the batch is provably unwritten.
          return inputRejected(
            command,
            workerCall.expired
              ? "input budget expired before worker send"
              : "worker unavailable",
          );
        }
        // Ordering into the worker is fixed by the completed write; the next
        // command may send while this batch's result finalizes.
        releaseLane();

        let outcome: InputControlResult;
        try {
          const result = await workerCall.result;
          if (result.sessionId !== command.sessionId || result.inputSeq !== command.inputSeq) {
            outcome = {
              status: "ambiguous",
              sessionId: command.sessionId,
              inputSeq: command.inputSeq,
              writtenBytes: 0,
              reason: "mismatched worker input result",
            };
          } else {
            outcome = classifyWorkerInput(command, result);
          }
        } catch (error) {
          outcome = {
            status: "ambiguous",
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            writtenBytes: 0,
            reason: (error instanceof Error ? error.message : "input result unavailable").slice(0, 200),
          };
        }
        return outcome;
      } catch (error) {
        const reason = error instanceof Error
          ? error.message
          : "coordinator is not write-active";
        // Past admission the batch may already be on the PTY, so a late
        // failure can never be downgraded into a retryable rejection.
        if (admitted) {
          return {
            status: "ambiguous",
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            writtenBytes: 0,
            reason: reason.slice(0, 200),
          };
        }
        return inputRejected(command, reason);
      } finally {
        lease?.release();
      }
    },
    () => inputRejected(command, "generation closed or control queue full"),
  ));
}

let compatibilityInputSeq = 0n;
export function nextCompatibilityInputSeq(): bigint {
  compatibilityInputSeq += 1n;
  return compatibilityInputSeq;
}
