// Shared terminal-write orchestration owns the sender/session FIFO, write-gate
// lease ordering, monotonic hop deadline, and WInputResult classification.
// Raw terminal input and status-fenced prompts supply only their worker sender
// and acceptance policy, keeping retries and ambiguity semantics identical.

import { AGENT_PROMPT_MAX_REASON_LENGTH } from "@roost/shared/terminal-input";
import {
  TerminalInputStatus,
  TerminalWritePhase,
  type WInputResult,
} from "@roost/shared/proto/worker_transport_pb";
import type { WriteLease } from "../coordinator-write-gate.ts";
import type { ConnectDeps } from "./router.ts";
import {
  INPUT_CONTROL_TIMEOUT_MS,
  startHopDeadline,
  type HopDeadline,
  type TerminalWorkerRequest,
} from "./worker-send.ts";
import {
  enqueueLane,
  resolveSessionRoute,
  type TerminalControlGeneration,
  type TerminalViewerIdentity,
} from "./terminal-control-lane.ts";


export type TerminalWriteControlResult =
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

export interface TerminalWriteControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  inputSeq: bigint;
  socketGeneration?: TerminalControlGeneration;
  /** Monotonic budget shared by queueing and the worker hop. */
  deadline?: HopDeadline;
}

export type TerminalWriteAcceptance =
  | { kind: "exact-bytes"; writtenBytes: number }
  | { kind: "worker-written"; maximumWrittenBytes: number };

export type TerminalWriteSender = (
  workerFp: string,
  deadline: HopDeadline,
) => TerminalWorkerRequest<WInputResult>;

export function terminalWriteRejected(
  command: Pick<TerminalWriteControlCommand, "sessionId" | "inputSeq">,
  reason: string,
): TerminalWriteControlResult {
  return {
    status: "rejected",
    sessionId: command.sessionId,
    inputSeq: command.inputSeq,
    writtenBytes: 0,
    reason: reason.slice(0, AGENT_PROMPT_MAX_REASON_LENGTH),
  };
}

function maximumWrittenBytes(acceptance: TerminalWriteAcceptance): number {
  return acceptance.kind === "exact-bytes"
    ? acceptance.writtenBytes
    : acceptance.maximumWrittenBytes;
}

function boundedWrittenBytes(
  result: WInputResult,
  maximum: number,
): number {
  return Number.isSafeInteger(result.writtenBytes)
    ? Math.max(0, Math.min(result.writtenBytes, maximum))
    : 0;
}

function workerResultAccepted(
  result: WInputResult,
  acceptance: TerminalWriteAcceptance,
): boolean {
  if (result.status !== TerminalInputStatus.ACCEPTED) return false;
  if (acceptance.kind === "exact-bytes") {
    // Raw SessionsInput predates write-phase proof and retains its exact-byte
    // acceptance rule unchanged.
    return Number.isSafeInteger(result.writtenBytes)
      && result.writtenBytes === acceptance.writtenBytes;
  }
  return result.phase === TerminalWritePhase.WRITTEN
    && Number.isSafeInteger(result.writtenBytes)
    && result.writtenBytes > 0
    && result.writtenBytes <= acceptance.maximumWrittenBytes;
}

function classifyWorkerResult(
  command: TerminalWriteControlCommand,
  result: WInputResult,
  acceptance: TerminalWriteAcceptance,
): TerminalWriteControlResult {
  const writtenBytes = boundedWrittenBytes(result, maximumWrittenBytes(acceptance));
  if (workerResultAccepted(result, acceptance)) {
    return {
      status: "accepted",
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      writtenBytes,
    };
  }
  if (
    result.status === TerminalInputStatus.REJECTED
    && result.phase === TerminalWritePhase.PRE_WRITE
    && result.writtenBytes === 0
  ) {
    return terminalWriteRejected(command, result.reason || "keeper rejected input");
  }
  return {
    status: "ambiguous",
    sessionId: command.sessionId,
    inputSeq: command.inputSeq,
    writtenBytes,
    reason: (result.reason || "input completion could not be proven")
      .slice(0, AGENT_PROMPT_MAX_REASON_LENGTH),
  };
}

/** Route one logical PTY write exactly once. A result after worker admission can
 * only be accepted or ambiguous; the coordinator never retries it. */
export function processTerminalWriteControl(
  deps: ConnectDeps,
  command: TerminalWriteControlCommand,
  acceptance: TerminalWriteAcceptance,
  sendToWorker: TerminalWriteSender,
): Promise<TerminalWriteControlResult> {
  const socketGeneration = command.socketGeneration ?? 0;
  const deadline = command.deadline ?? startHopDeadline(INPUT_CONTROL_TIMEOUT_MS);
  return enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async (releaseLane) => {
      let lease: WriteLease | null = null;
      let admitted = false;
      try {
        // Inside the lane, never before it: leasing earlier would let queued
        // input hold the exclusive keeper-update drain open forever.
        lease = deps.writeGate.acquire();
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return terminalWriteRejected(command, "unknown session");
        const workerCall = sendToWorker(route.workerFp, deadline);
        admitted = workerCall.admitted;
        if (!admitted) {
          void workerCall.result.catch(() => undefined);
          return terminalWriteRejected(
            command,
            workerCall.expired
              ? "input budget expired before worker send"
              : "worker unavailable",
          );
        }
        // Socket order is fixed at admission. The worker's keeper lane owns the
        // remaining FIFO while this request waits for its write proof.
        releaseLane();
        try {
          const result = await workerCall.result;
          if (result.sessionId !== command.sessionId || result.inputSeq !== command.inputSeq) {
            return {
              status: "ambiguous",
              sessionId: command.sessionId,
              inputSeq: command.inputSeq,
              writtenBytes: 0,
              reason: "mismatched worker input result",
            };
          }
          return classifyWorkerResult(command, result, acceptance);
        } catch (error) {
          return {
            status: "ambiguous",
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            writtenBytes: 0,
            reason: (error instanceof Error ? error.message : "input result unavailable")
              .slice(0, AGENT_PROMPT_MAX_REASON_LENGTH),
          };
        }
      } catch (error) {
        const reason = error instanceof Error
          ? error.message
          : "coordinator write lease unavailable";
        if (admitted) {
          return {
            status: "ambiguous",
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            writtenBytes: 0,
            reason: reason.slice(0, AGENT_PROMPT_MAX_REASON_LENGTH),
          };
        }
        return terminalWriteRejected(command, reason);
      } finally {
        lease?.release();
      }
    },
    () => terminalWriteRejected(command, "generation closed or control queue full"),
  );
}
