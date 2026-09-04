// Shared coordinator substrate for terminal input admission. It owns the
// tab-scoped sender identity, bounded per-sender/session FIFO, Sync-generation
// cancellation, and live session-route resolution. Terminal view membership
// and SCD are owned exclusively by TerminalViewHub and never enter this lane.

import type { KyselyDB } from "../db/connection.ts";
import {
  cacheSessionWorker,
  getCachedSessionWorker,
  isWorkerChannelIndexReconciled,
} from "../byte-hub.ts";

const MAX_LANE_DEPTH = 256;
const MAX_AGGREGATE_DEPTH = 2_048;

export interface TerminalViewerIdentity {
  viewerKey: string;
  callerFingerprint: string;
  /** Dashboard selected by a server-resolved actor; absent identities are
   * intentionally unable to route terminal input. */
  dashboardId?: string;
  clientIp?: string;
}

export type TerminalControlGeneration = number | string;

/** Build the sender identity used by unary compatibility and Sync v2.
 * The remote address remains metadata rather than part of the stable key: a
 * tailnet address change must not mint a second browser sender. */
export function terminalViewerIdentity(
  callerFingerprint: string,
  tabId: string | null | undefined,
  remoteAddress?: string,
  dashboardId?: string,
): TerminalViewerIdentity {
  return {
    viewerKey: tabId ? `${callerFingerprint}:${tabId}` : callerFingerprint,
    callerFingerprint,
    clientIp: remoteAddress,
    dashboardId,
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
let aggregateDepth = 0;

export const laneKey = (viewerKey: string, sessionId: string): string =>
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

/** Run one input command behind the per-sender/session lane. `run` is handed a
 * one-shot `releaseLane`: calling it lets the next queued command start while
 * this one keeps finalizing. Input releases at worker-send admission, preserving
 * coordinator socket order while the worker's channel/keeper lane owns the
 * authoritative FIFO and result proof. Depth counts through settlement. */
export function enqueueLane<T>(
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
  dashboardId: string;
  channel: number;
}

export async function resolveSessionRoute(
  db: KyselyDB,
  dashboardId: string | undefined,
  sessionId: string,
): Promise<SessionRoute | null> {
  if (dashboardId === undefined) return null;
  // Always prove persisted scope before consulting the process-global route
  // cache. A guessed foreign id must not create a cache/view/worker side effect.
  const row = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select([
      "session.worker_fp as worker_fp",
      "session.channel as channel",
    ])
    .where("session.id", "=", sessionId)
    .where("session.dashboard_id", "=", dashboardId)
    .where("session.status", "=", "open")
    .where("worker.dashboard_id", "=", dashboardId)
    .where("worker.deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!row) return null;
  const cached = getCachedSessionWorker(sessionId);
  if (
    cached
    && cached.worker_fp === row.worker_fp
    && cached.channel === row.channel
  ) return {
    workerFp: cached.worker_fp,
    channel: cached.channel,
    dashboardId,
  };
  // DB fallback exists only for the pre-reconcile startup window (coord
  // restarted under a live worker, before that worker's snapshot arrived). Once
  // the worker announced its exact live set, a session with no live route is
  // offline: re-caching the open breadcrumb here used to resurrect the
  // stale channel, so input was written into a channel no keeper owned.
  if (isWorkerChannelIndexReconciled(row.worker_fp)) return null;
  cacheSessionWorker(sessionId, row.worker_fp, row.channel);
  return { workerFp: row.worker_fp, channel: row.channel, dashboardId };
}
