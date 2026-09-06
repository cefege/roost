// Terminal input control: one logical PTY input batch per bounded sender lane,
// with the audit-log queue that persists its outcome. Terminal view membership
// and SCD are intentionally absent; input admission remains session-scoped.

import { signal } from "@roost/shared/diag";
import type { ConnectDeps } from "./router.ts";
import {
  sendTerminalInputRequest,
  type HopDeadline,
} from "./worker-send.ts";
import {
  type TerminalControlGeneration,
  type TerminalViewerIdentity,
} from "./terminal-control-lane.ts";
import {
  processTerminalWriteControl,
  terminalWriteRejected,
  type TerminalWriteControlResult,
} from "./terminal-write-control.ts";
import { writeAuditLog } from "../middleware/security.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const INPUT_AUDIT_QUEUE_CAP = 1_024;

export type InputControlResult = TerminalWriteControlResult;

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


interface InputAuditRecord {
  deps: ConnectDeps;
  callerFingerprint: string;
  outcome: InputControlResult["status"];
  dashboardId: string;
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
            dashboardId: next.dashboardId,
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
          dashboardId: command.identity.dashboardId!,
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
  if (command.identity.dashboardId === undefined) {
    return finish(Promise.resolve(terminalWriteRejected(
      command,
      "terminal dashboard scope is unavailable",
    )));
  }
  if (command.data.byteLength === 0) {
    return finish(Promise.resolve({
      status: "accepted",
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      writtenBytes: 0,
    }));
  }
  if (command.data.byteLength > MAX_INPUT_BYTES) {
    return finish(Promise.resolve(terminalWriteRejected(command, "input exceeds 64 KiB")));
  }
  // Protobuf byte fields may view a recycled transport buffer. The FIFO can
  // outlive its handler turn, so ownership must transfer before queue entry.
  const ownedData = command.data.slice();
  return finish(processTerminalWriteControl(
    deps,
    command,
    { kind: "exact-bytes", writtenBytes: ownedData.byteLength },
    (workerFp, dashboardId, deadline) => sendTerminalInputRequest(workerFp, {
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      data: ownedData,
      dashboardId,
    }, deadline),
  ));
}

let compatibilityInputSeq = 0n;
export function nextCompatibilityInputSeq(): bigint {
  compatibilityInputSeq += 1n;
  return compatibilityInputSeq;
}
