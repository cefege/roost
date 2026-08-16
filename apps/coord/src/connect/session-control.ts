import { signal } from "@roost/shared/diag";
import {
  TerminalInputStatus,
  TerminalViewportStatus,
} from "@roost/shared/proto/worker_transport_pb";
import type { KyselyDB } from "../db/connection.ts";
import type { ConnectDeps } from "./router.ts";
import { cacheSessionWorker, getCachedSessionWorker } from "../byte-hub.ts";
import { mutateCellSubscription } from "./cell-subscriptions.ts";
import { _bumpViewer } from "./viewer-tracker.ts";
import {
  sendTerminalInputRequest,
  sendTerminalViewportRequest,
} from "./worker-send.ts";
import { writeAuditLog } from "../middleware/security.ts";
import type { WriteLease } from "../coord-move/write-gate.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_LANE_DEPTH = 256;
const MAX_AGGREGATE_DEPTH = 2_048;
const INPUT_AUDIT_QUEUE_CAP = 1_024;

export interface TerminalViewerIdentity {
  viewerKey: string;
  callerFingerprint: string;
  clientIp?: string;
}

export type TerminalControlGeneration = number | string;

/** Build the one viewer identity used by unary compatibility and Sync v2.
 * The remote address remains metadata rather than part of the stable key: a
 * tailnet address change must not mint a second viewport claim for one tab. */
export function terminalViewerIdentity(
  callerFingerprint: string,
  tabId: string | null | undefined,
  remoteAddress?: string,
): TerminalViewerIdentity {
  return {
    viewerKey: tabId ? `${callerFingerprint}:${tabId}` : callerFingerprint,
    callerFingerprint,
    clientIp: remoteAddress,
  };
}

interface Lane {
  tail: Promise<void>;
  depth: number;
}

interface GenerationQueueState {
  queued: number;
  canceled: boolean;
}

const lanes = new Map<string, Lane>();
const generationQueues = new Map<string, GenerationQueueState>();
interface CommittedViewport {
  browserClientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  result: Extract<ViewportControlResult, { status: "accepted" }>;
}

// Only a typed worker COMMITTED result enters this cache. It distinguishes an
// exact replay whose browser control was lost from a stale/equal-conflicting
// command rejected by mutateCellSubscription's watermark.
const committedViewports = new Map<string, CommittedViewport>();
let aggregateDepth = 0;

const laneKey = (viewerKey: string, sessionId: string): string =>
  `${viewerKey}\u0000${sessionId}`;
const generationKey = (
  viewerKey: string,
  socketGeneration: TerminalControlGeneration,
): string =>
  `${viewerKey}\u0000${socketGeneration}`;

/** Cancel commands that have not begun on a closing Sync generation. A command
 * already inside its lane keeps running; the lane tail makes the first command
 * from a replacement generation wait behind it. */
export function cancelTerminalControlGeneration(
  viewerKey: string,
  socketGeneration: TerminalControlGeneration,
): void {
  if (typeof socketGeneration === "number" && socketGeneration <= 0) return;
  if (socketGeneration === "") return;
  const state = generationQueues.get(generationKey(viewerKey, socketGeneration));
  if (state) state.canceled = true;
}

function generationWasCanceled(
  viewerKey: string,
  socketGeneration: TerminalControlGeneration,
): boolean {
  const hasGeneration = typeof socketGeneration === "string"
    ? socketGeneration.length > 0
    : socketGeneration > 0;
  return hasGeneration
    && generationQueues.get(generationKey(viewerKey, socketGeneration))?.canceled === true;
}

function enqueueLane<T>(
  viewerKey: string,
  sessionId: string,
  socketGeneration: TerminalControlGeneration,
  run: () => Promise<T>,
  rejected: () => T,
): Promise<T> {
  const key = laneKey(viewerKey, sessionId);
  let lane = lanes.get(key);
  if (!lane) {
    lane = { tail: Promise.resolve(), depth: 0 };
    lanes.set(key, lane);
  }
  if (lane.depth >= MAX_LANE_DEPTH || aggregateDepth >= MAX_AGGREGATE_DEPTH) {
    return Promise.resolve(rejected());
  }

  lane.depth += 1;
  aggregateDepth += 1;
  const hasGeneration = typeof socketGeneration === "string"
    ? socketGeneration.length > 0
    : socketGeneration > 0;
  const queuedGenerationKey = hasGeneration
    ? generationKey(viewerKey, socketGeneration)
    : null;
  let generationState: GenerationQueueState | null = null;
  if (queuedGenerationKey) {
    generationState = generationQueues.get(queuedGenerationKey)
      ?? { queued: 0, canceled: false };
    generationState.queued += 1;
    generationQueues.set(queuedGenerationKey, generationState);
  }
  const task = lane.tail.then(() => {
    if (generationWasCanceled(viewerKey, socketGeneration)) return rejected();
    return run();
  });
  const settled = task.then(() => undefined, () => undefined).finally(() => {
    lane!.depth -= 1;
    aggregateDepth -= 1;
    if (lane!.depth === 0 && lanes.get(key) === lane) lanes.delete(key);
    if (generationState && queuedGenerationKey) {
      generationState.queued -= 1;
      if (generationState.queued === 0
        && generationQueues.get(queuedGenerationKey) === generationState) {
        generationQueues.delete(queuedGenerationKey);
      }
    }
  });
  lane.tail = settled;
  return task;
}

interface SessionRoute {
  workerFp: string;
  channel: number;
}

async function resolveSessionRoute(db: KyselyDB, sessionId: string): Promise<SessionRoute | null> {
  const cached = getCachedSessionWorker(sessionId);
  if (cached) return { workerFp: cached.worker_fp, channel: cached.channel };
  const row = await db.selectFrom("sessions")
    .select(["worker_fp", "channel"])
    .where("id", "=", sessionId)
    .where("status", "=", "open")
    .executeTakeFirst();
  if (!row) return null;
  cacheSessionWorker(sessionId, row.worker_fp, row.channel);
  return { workerFp: row.worker_fp, channel: row.channel };
}

export type ViewportControlResult =
  | {
      status: "accepted";
      sessionId: string;
      clientSeq: bigint;
      channelResizeSeq: bigint;
      cols: number;
      rows: number;
      resized: boolean;
    }
  | {
      status: "rejected";
      sessionId: string;
      clientSeq: bigint;
      reason: string;
    };

export interface ViewportControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  clientSeq: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq?: bigint;
  socketGeneration?: TerminalControlGeneration;
}

const viewportRejected = (
  command: Pick<ViewportControlCommand, "sessionId" | "clientSeq">,
  reason: string,
): ViewportControlResult => ({
  status: "rejected",
  sessionId: command.sessionId,
  clientSeq: command.clientSeq,
  reason: reason.slice(0, 200),
});

/** Apply one viewport command under the shared per-viewer/session lane. The
 * subscription mutation is rolled back by identity unless the worker returns a
 * typed committed result; a missing or late result can never retire the
 * browser's intent. */
export function processViewportControl(
  deps: ConnectDeps,
  command: ViewportControlCommand,
): Promise<ViewportControlResult> {
  const socketGeneration = command.socketGeneration ?? 0;
  return enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async () => {
      let lease: WriteLease | null = null;
      try {
        lease = deps.move?.gate.acquire() ?? null;
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return viewportRejected(command, "unknown session");

        const withdrawsCells = command.cause !== 5
          && (command.cols <= 0 || command.rows <= 0);
        const mutation = mutateCellSubscription(
          command.identity.viewerKey,
          command.sessionId,
          !withdrawsCells,
          command.clientSeq,
          command.cause === 6,
        );
        if (!mutation) {
          const committed = committedViewports.get(
            laneKey(command.identity.viewerKey, command.sessionId),
          );
          if (
            committed
            && committed.browserClientSeq === command.clientSeq
            && committed.cols === command.cols
            && committed.rows === command.rows
            && committed.cause === command.cause
            && committed.heldCellSeq === (command.heldCellSeq ?? 0n)
          ) {
            return committed.result;
          }
          return viewportRejected(command, "stale or conflicting viewport intent");
        }

        const effectiveClientSeq = mutation.effectiveClientSeq;
        const workerCall = sendTerminalViewportRequest(route.workerFp, {
          sessionId: command.sessionId,
          viewerId: command.identity.viewerKey,
          clientSeq: effectiveClientSeq,
          cols: command.cols,
          rows: command.rows,
          cause: command.cause,
          heldCellSeq: command.heldCellSeq ?? 0n,
        });
        if (!workerCall.admitted) {
          void workerCall.result.catch(() => undefined);
          mutation.rollback();
          return viewportRejected(command, "worker unavailable");
        }

        try {
          const result = await workerCall.result;
          if (
            result.status !== TerminalViewportStatus.COMMITTED
            || result.sessionId !== command.sessionId
            || result.clientSeq !== effectiveClientSeq
          ) {
            mutation.rollback();
            return viewportRejected(command, result.reason || "worker rejected viewport");
          }
          _bumpViewer(
            command.sessionId,
            command.identity.viewerKey,
            command.cols,
            command.rows,
            Number(effectiveClientSeq),
            command.identity.clientIp,
            command.identity.callerFingerprint,
          );
          const accepted: Extract<ViewportControlResult, { status: "accepted" }> = {
            status: "accepted",
            sessionId: command.sessionId,
            clientSeq: command.clientSeq,
            channelResizeSeq: result.channelResizeSeq,
            cols: result.cols,
            rows: result.rows,
            resized: result.resized,
          };
          committedViewports.set(
            laneKey(command.identity.viewerKey, command.sessionId),
            {
              browserClientSeq: command.clientSeq,
              cols: command.cols,
              rows: command.rows,
              cause: command.cause,
              heldCellSeq: command.heldCellSeq ?? 0n,
              result: accepted,
            },
          );
          return accepted;
        } catch (error) {
          mutation.rollback();
          return viewportRejected(
            command,
            error instanceof Error ? error.message : "viewport result unavailable",
          );
        }
      } catch (error) {
        return viewportRejected(
          command,
          error instanceof Error ? error.message : "coordinator is not write-active",
        );
      } finally {
        lease?.release();
      }
    },
    () => viewportRejected(command, "generation closed or control queue full"),
  );
}

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
  status: TerminalInputStatus,
  writtenBytes: number,
  reason: string,
): InputControlResult {
  const written = Number.isSafeInteger(writtenBytes)
    ? Math.max(0, Math.min(writtenBytes, command.data.byteLength))
    : 0;
  if (status === TerminalInputStatus.ACCEPTED && written === command.data.byteLength) {
    return {
      status: "accepted",
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      writtenBytes: written,
    };
  }
  if (status === TerminalInputStatus.REJECTED && writtenBytes === 0) {
    return inputRejected(command, reason || "keeper rejected input");
  }
  return {
    status: "ambiguous",
    sessionId: command.sessionId,
    inputSeq: command.inputSeq,
    writtenBytes: written,
    reason: (reason || "input completion could not be proven").slice(0, 200),
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
  return finish(enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async () => {
      let lease: WriteLease | null = null;
      try {
        lease = deps.move?.gate.acquire() ?? null;
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return inputRejected(command, "unknown session");
        const workerCall = sendTerminalInputRequest(route.workerFp, {
          sessionId: command.sessionId,
          inputSeq: command.inputSeq,
          data: command.data,
        });
        if (!workerCall.admitted) {
          void workerCall.result.catch(() => undefined);
          return inputRejected(command, "worker unavailable");
        }

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
            outcome = classifyWorkerInput(
              command,
              result.status,
              result.writtenBytes,
              result.reason,
            );
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
        return inputRejected(
          command,
          error instanceof Error ? error.message : "coordinator is not write-active",
        );
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

export function _terminalControlQueueStats(): {
  lanes: number;
  depth: number;
  auditDepth: number;
} {
  return { lanes: lanes.size, depth: aggregateDepth, auditDepth: inputAuditQueue.length };
}
