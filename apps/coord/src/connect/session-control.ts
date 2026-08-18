import { signal } from "@roost/shared/diag";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import {
  TerminalInputStatus,
  TerminalViewportStatus,
  TerminalWritePhase,
  type WInputResult,
} from "@roost/shared/proto/worker_transport_pb";
import type { KyselyDB } from "../db/connection.ts";
import type { ConnectDeps } from "./router.ts";
import {
  cacheSessionWorker,
  getCachedSessionWorker,
  isBarrierRepairMarked,
  isWorkerChannelIndexReconciled,
} from "../byte-hub.ts";
import {
  mutateCellSubscription,
  subscribedCellSeq,
  type CellSubscriptionMutation,
} from "./cell-subscriptions.ts";
import {
  mutateViewer,
  _viewersBySession,
  type ViewerMutation,
} from "./viewer-tracker.ts";
import {
  sendTerminalInputRequest,
  sendTerminalViewportRequest,
  startHopDeadline,
  INPUT_CONTROL_TIMEOUT_MS,
  VIEWPORT_CONTROL_TIMEOUT_MS,
  type HopDeadline,
} from "./worker-send.ts";
import { writeAuditLog } from "../middleware/security.ts";
import type { WriteLease } from "../coord-move/write-gate.ts";

const MAX_INPUT_BYTES = 64 * 1024;
const MAX_LANE_DEPTH = 256;
const MAX_AGGREGATE_DEPTH = 2_048;
const INPUT_AUDIT_QUEUE_CAP = 1_024;
const COMMITTED_VIEWPORT_CACHE_CAP = 8_192;

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
  cellMutation: CellSubscriptionMutation;
  viewerMutation: ViewerMutation;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  result: Extract<ViewportControlResult, { status: "accepted" }>;
}

// Only a matching typed worker COMMITTED result enters this bounded cache.
// Exact replay is allowed only while both installed mutation identities remain
// current, including across legacy zero-sequence withdrawal semantics.
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

/** Run one command behind the per-viewer/session lane. `run` is handed a
 * one-shot `releaseLane`: calling it lets the next queued command start while
 * this one keeps finalizing. Both viewport and input release at worker-send
 * admission, so the socket write order that the worker's keeper-admission lane
 * depends on is still fixed here, but a viewport parked on a keeper resize no
 * longer holds the following PTY input hostage to its own result. Depth counts
 * the command until it truly settles, so the lane stays bounded either way. */
function enqueueLane<T>(
  viewerKey: string,
  sessionId: string,
  socketGeneration: TerminalControlGeneration,
  run: (releaseLane: () => void) => Promise<T>,
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
  const handoff = Promise.withResolvers<void>();
  const task = lane.tail.then(() => {
    if (generationWasCanceled(viewerKey, socketGeneration)) return rejected();
    return run(handoff.resolve);
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
  // A command that never releases (rejected before admission) still gates the
  // lane on its own settlement, so no ordering is lost when a send is refused.
  lane.tail = Promise.race([handoff.promise, settled]);
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
  // DB fallback exists only for the pre-reconcile startup window (coord
  // restarted under a live worker, before that worker's snapshot arrived). Once
  // the worker announced its exact live set, a session with no live route is
  // offline: re-caching the open breadcrumb here used to resurrect the
  // pre-restart channel, so input and claims were written into a channel no
  // keeper owned and never produced output.
  if (isWorkerChannelIndexReconciled(row.worker_fp)) return null;
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
    }
  | {
      status: "ambiguous";
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
  /** Monotonic hop budget shared by the lane wait and the worker send.
   * Injected by tests; production starts it at entry. */
  deadline?: HopDeadline;
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

const viewportAmbiguous = (
  command: Pick<ViewportControlCommand, "sessionId" | "clientSeq">,
  reason: string,
): ViewportControlResult => ({
  status: "ambiguous",
  sessionId: command.sessionId,
  clientSeq: command.clientSeq,
  reason: reason.slice(0, 200),
});

/** Apply one viewport command under the shared per-viewer/session lane. Cell
 * and viewer membership become provisional before transport admission. A
 * definite non-admission, a pre-send budget expiry, or a typed REJECTED result
 * whose phase proves the keeper never wrote rolls both back by identity; every
 * other post-admission outcome remains TTL-backed and is reported as ambiguous
 * for a newer monotonic intent to reconcile. */
export function processViewportControl(
  deps: ConnectDeps,
  command: ViewportControlCommand,
): Promise<ViewportControlResult> {
  const socketGeneration = command.socketGeneration ?? 0;
  // The budget starts here, before the lane wait, so a command that queues
  // behind a slow predecessor spends its own time rather than being handed a
  // fresh full budget at send.
  const deadline = command.deadline ?? startHopDeadline(VIEWPORT_CONTROL_TIMEOUT_MS);
  return enqueueLane(
    command.identity.viewerKey,
    command.sessionId,
    socketGeneration,
    async (releaseLane) => {
      let lease: WriteLease | null = null;
      let cellMutation: CellSubscriptionMutation | null = null;
      let viewerMutation: ViewerMutation | null = null;
      let admitted = false;
      try {
        lease = deps.move?.gate.acquire() ?? null;
        const route = await resolveSessionRoute(deps.db, command.sessionId);
        if (!route) return viewportRejected(command, "unknown session");

        // Barrier repair is coordinator-local and exact by route: the
        // announcement barrier dropped this (worker, session, channel)'s cells,
        // so the browser's held sequence cannot prove it is current even though
        // the browser has no way to know that. Override the worker-bound value
        // to zero — no browser protocol field — until a full frame publishes for
        // this exact route; later claims carry the browser's own value again.
        const heldCellSeq = isBarrierRepairMarked(
          route.workerFp,
          command.sessionId,
          route.channel,
        )
          ? 0n
          : command.heldCellSeq ?? 0n;

        const subscribes = command.cause === 5
          || (command.cols > 0 && command.rows > 0);
        const refreshEqual = command.cause === 6;
        cellMutation = mutateCellSubscription(
          command.identity.viewerKey,
          command.sessionId,
          subscribes,
          command.clientSeq,
          refreshEqual,
        );
        if (!cellMutation) {
          const committed = committedViewports.get(
            laneKey(command.identity.viewerKey, command.sessionId),
          );
          if (
            committed
            && committed.browserClientSeq === command.clientSeq
            && committed.cols === command.cols
            && committed.rows === command.rows
            && committed.cause === command.cause
            && committed.heldCellSeq === heldCellSeq
            && committed.cellMutation.isCurrent()
            && committed.viewerMutation.isCurrent()
          ) {
            return committed.result;
          }
          return viewportRejected(command, "stale or conflicting viewport intent");
        }

        const effectiveClientSeq = cellMutation.effectiveClientSeq;
        viewerMutation = mutateViewer(
          command.sessionId,
          command.identity.viewerKey,
          subscribes,
          command.cols,
          command.rows,
          command.clientSeq,
          refreshEqual,
          command.identity.clientIp,
          command.identity.callerFingerprint,
        );
        if (
          !viewerMutation
          || viewerMutation.effectiveClientSeq !== effectiveClientSeq
        ) {
          viewerMutation?.rollback();
          cellMutation.rollback();
          return viewportRejected(command, "stale or conflicting viewer intent");
        }

        const workerCall = sendTerminalViewportRequest(route.workerFp, {
          sessionId: command.sessionId,
          viewerId: command.identity.viewerKey,
          clientSeq: effectiveClientSeq,
          cols: command.cols,
          rows: command.rows,
          cause: command.cause,
          heldCellSeq,
        }, deadline);
        admitted = workerCall.admitted;
        if (!admitted) {
          void workerCall.result.catch(() => undefined);
          viewerMutation.rollback();
          cellMutation.rollback();
          // Neither branch reached the socket, so no PTY state can exist and a
          // retry cannot duplicate: both are definite rejections.
          return viewportRejected(
            command,
            workerCall.expired
              ? "viewport budget expired before worker send"
              : "worker unavailable",
          );
        }
        // The frame is on the current worker socket, so the receive order the
        // worker's keeper-admission lane depends on is already fixed. Let the
        // next command send while this result finalizes.
        releaseLane();

        const result = await workerCall.result;
        if (
          result.requestId !== workerCall.requestId
          || result.sessionId !== command.sessionId
          || result.clientSeq !== effectiveClientSeq
        ) {
          return viewportAmbiguous(command, "mismatched worker viewport result");
        }

        switch (result.status) {
          case TerminalViewportStatus.REJECTED:
            // A rejection may only unwind provisional membership when the
            // worker proves it stopped before the keeper write. Any other
            // phase means a mutation could exist, so it stays ambiguous and
            // convergence is left to a newer claim rather than a rollback.
            if (result.phase !== TerminalWritePhase.PRE_WRITE) {
              return viewportAmbiguous(
                command,
                result.reason || "worker rejected without proving no resize",
              );
            }
            viewerMutation.rollback();
            cellMutation.rollback();
            return viewportRejected(
              command,
              result.reason || "keeper rejected viewport",
            );
          case TerminalViewportStatus.AMBIGUOUS:
            return viewportAmbiguous(
              command,
              result.reason || "worker could not prove viewport outcome",
            );
          case TerminalViewportStatus.UNSPECIFIED:
            return viewportAmbiguous(
              command,
              result.reason || "worker returned no viewport outcome",
            );
          case TerminalViewportStatus.COMMITTED: {
            if (!cellMutation.isCurrent() || !viewerMutation.isCurrent()) {
              return viewportAmbiguous(
                command,
                "viewport ownership changed before worker commit",
              );
            }
            const accepted: Extract<ViewportControlResult, { status: "accepted" }> = {
              status: "accepted",
              sessionId: command.sessionId,
              clientSeq: command.clientSeq,
              channelResizeSeq: result.channelResizeSeq,
              cols: result.cols,
              rows: result.rows,
              resized: result.resized,
            };
            const committedKey = laneKey(
              command.identity.viewerKey,
              command.sessionId,
            );
            if (
              !committedViewports.has(committedKey)
              && committedViewports.size >= COMMITTED_VIEWPORT_CACHE_CAP
            ) {
              const oldestKey = committedViewports.keys().next().value;
              if (oldestKey !== undefined) committedViewports.delete(oldestKey);
            }
            // Reinsertion makes replacement order deterministic for bounded
            // oldest-first eviction without another recency registry.
            committedViewports.delete(committedKey);
            committedViewports.set(committedKey, {
              browserClientSeq: command.clientSeq,
              cellMutation,
              viewerMutation,
              cols: command.cols,
              rows: command.rows,
              cause: command.cause,
              heldCellSeq,
              result: accepted,
            });
            return accepted;
          }
          default:
            return viewportAmbiguous(
              command,
              result.reason || "worker returned an unknown viewport outcome",
            );
        }
      } catch (error) {
        if (admitted) {
          return viewportAmbiguous(
            command,
            error instanceof Error ? error.message : "viewport result unavailable",
          );
        }
        viewerMutation?.rollback();
        cellMutation?.rollback();
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

export interface BarrierRepairRoute {
  workerFp: string;
  sessionId: string;
  channelId: number;
}

export interface BarrierRepairReplay {
  /** Refreshes enqueued; 0 = nobody is watching this session yet. */
  enqueued: number;
  /** Settles once every enqueued refresh has left the coordinator lane. */
  settled: Promise<void>;
}

/** The announcement barrier lost cell frames for one exact route. Replay a
 *  HEARTBEAT-shaped claim for every viewer that is actually watching, at the
 *  watermark the coordinator already installed and with `held_cell_seq = 0`: the
 *  worker's stale-sequence path answers with one authoritative full frame, so
 *  recovery never waits for an unrelated delta or a browser reload. With nobody
 *  watching, the standing repair mark alone covers that tab's next claim.
 *
 *  Membership, geometry, and ordering stay untouched — the refresh runs on the
 *  same per-viewer lane as browser intents, carries no new intent, and never
 *  advances the browser's watermark. Background panes at 0×0 are skipped: a
 *  positive-cause claim would read as a withdraw, and their own next visible
 *  claim picks up the override. No write lease is taken because nothing here
 *  mutates coordinator state. */
export function requestBarrierRepairFullFrame(route: BarrierRepairRoute): BarrierRepairReplay {
  const viewers = _viewersBySession.get(route.sessionId);
  if (!viewers) return { enqueued: 0, settled: Promise.resolve() };
  const replays: Array<Promise<unknown>> = [];
  for (const viewerKey of viewers.keys()) {
    const clientSeq = subscribedCellSeq(viewerKey, route.sessionId);
    if (clientSeq === null) continue;
    const geometry = viewers.get(viewerKey);
    if (!geometry || geometry.cols <= 0 || geometry.rows <= 0) continue;
    replays.push(enqueueLane(
      viewerKey,
      route.sessionId,
      0,
      async (releaseLane) => {
        // Re-read at send time: a withdraw or resize may have overtaken the
        // drop while this refresh waited for the lane.
        const current = _viewersBySession.get(route.sessionId)?.get(viewerKey);
        const seq = subscribedCellSeq(viewerKey, route.sessionId);
        if (!current || seq === null || current.cols <= 0 || current.rows <= 0) {
          releaseLane();
          return false;
        }
        const request = sendTerminalViewportRequest(route.workerFp, {
          sessionId: route.sessionId,
          viewerId: viewerKey,
          clientSeq: seq,
          cols: current.cols,
          rows: current.rows,
          cause: ResizeCause.HEARTBEAT,
          heldCellSeq: 0n,
        });
        releaseLane();
        // The refresh carries no intent, so its only product is the worker's
        // full frame: a lost result changes nothing, and the standing mark keeps
        // the override until that frame actually publishes.
        void request.result.catch(() => undefined);
        return request.admitted;
      },
      () => false,
    ).catch(() => undefined));
  }
  return {
    enqueued: replays.length,
    settled: Promise.all(replays).then(() => undefined),
  };
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
  // Same monotonic origin rule as the viewport path: the budget starts before
  // the lane wait so queueing cannot mint a fresh one at send time.
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
