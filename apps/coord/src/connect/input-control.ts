// Terminal input control: one logical PTY input batch per bounded sender lane,
// with the audit-log queue that persists its outcome. Terminal view membership
// and SCD are intentionally absent; input admission remains session-scoped.

import { signal } from "@roost/observability/diag";
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
import {
  writeAuditLogs,
  type AuditLogOptions,
} from "../middleware/security.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const INPUT_AUDIT_QUEUE_CAP = 1_024;
const INPUT_AUDIT_BATCH_MAX = 64;

export type InputControlResult = TerminalWriteControlResult;

/** Authenticated browser actor and current route epoch carried to the worker. */
export interface InputRouteAuthority {
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly connectionId: string;
  readonly inputRouteEpoch: string;
}

export interface InputControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  inputSeq: bigint;
  data: Uint8Array;
  socketGeneration?: TerminalControlGeneration;
  /** Present only for Sync/browser input. Unary and worker-owned writers stay
   * outside browser route ownership and carry the legacy-empty envelope. */
  inputRouteAuthority?: InputRouteAuthority;
  audit?: { traceId?: string };
  /** Monotonic hop budget shared by the lane wait and the worker send.
   * Injected by tests; production starts it at entry. */
  deadline?: HopDeadline;
}


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
const inputAuditCapacityWaiters: QueuedInputAudit[] = [];
let inputAuditInFlight = 0;
let inputAuditPumping = false;


function refillInputAuditCapacity(): void {
  while (
    inputAuditQueue.length + inputAuditInFlight < INPUT_AUDIT_QUEUE_CAP
    && inputAuditCapacityWaiters.length > 0
  ) {
    inputAuditQueue.push(inputAuditCapacityWaiters.shift()!);
  }
}

function takeInputAuditBatch(): QueuedInputAudit[] {
  const first = inputAuditQueue.shift()!;
  const batch = [first];
  while (
    batch.length < INPUT_AUDIT_BATCH_MAX
    && inputAuditQueue[0]?.deps.db === first.deps.db
  ) {
    batch.push(inputAuditQueue.shift()!);
  }
  return batch;
}

function pumpInputAudits(): void {
  if (inputAuditPumping) return;
  inputAuditPumping = true;
  queueMicrotask(async () => {
    try {
      while (inputAuditQueue.length > 0) {
        const batch = takeInputAuditBatch();
        inputAuditInFlight += batch.length;
        try {
          const auditEntries: AuditLogOptions[] = [];
          for (const entry of batch) {
            auditEntries.push({
              db: entry.deps.db,
              status: entry.outcome === "accepted" ? 200 : entry.outcome === "ambiguous" ? 409 : 422,
              method: "SYNC",
              path: `/ws/coord-sync/input/${entry.outcome}/${entry.writtenBytes}/SessionsInput`,
              traceId: entry.traceId,
              callerFp: entry.callerFingerprint,
              throwOnFailure: true,
              dashboardId: entry.deps.selfHostedTenant.dashboardId,
            });
          }
          await writeAuditLogs(auditEntries);
          for (const entry of batch) entry.resolve();
        } catch (error) {
          for (const entry of batch) entry.reject(error);
        } finally {
          inputAuditInFlight -= batch.length;
          refillInputAuditCapacity();
        }
      }
    } finally {
      inputAuditPumping = false;
      if (inputAuditQueue.length > 0) pumpInputAudits();
    }
  });
}

function enqueueInputAudit(entry: InputAuditRecord): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const queued = { ...entry, resolve, reject };
    if (inputAuditQueue.length + inputAuditInFlight >= INPUT_AUDIT_QUEUE_CAP) {
      signal("audit.input_queue_backpressure", {
        caller_fp: entry.callerFingerprint,
        cooldownKey: "terminal-input",
      });
      inputAuditCapacityWaiters.push(queued);
      return;
    }
    inputAuditQueue.push(queued);
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
    return finish(Promise.resolve(terminalWriteRejected(command, "input exceeds 64 KiB")));
  }
  // Protobuf byte fields may view a recycled transport buffer. The FIFO can
  // outlive its handler turn, so ownership must transfer before queue entry.
  const ownedData = command.data.slice();
  return finish(processTerminalWriteControl(
    deps,
    command,
    { kind: "exact-bytes", writtenBytes: ownedData.byteLength },
    (workerFp, deadline) => sendTerminalInputRequest(workerFp, {
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      data: ownedData,
      deviceFingerprint: command.inputRouteAuthority?.deviceFingerprint ?? "",
      tabId: command.inputRouteAuthority?.tabId ?? "",
      browserConnectionId: command.inputRouteAuthority?.connectionId ?? "",
      inputRouteEpoch: command.inputRouteAuthority?.inputRouteEpoch ?? "",
    }, deadline),
  ));
}

let compatibilityInputSeq = 0n;
export function nextCompatibilityInputSeq(): bigint {
  compatibilityInputSeq += 1n;
  return compatibilityInputSeq;
}
