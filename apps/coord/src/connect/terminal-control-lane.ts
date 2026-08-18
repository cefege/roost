// Shared substrate for the two terminal control paths — viewport-control.ts and
// input-control.ts — split out of the former session-control.ts, which is now a
// re-export barrel. Holds the per-tab viewer identity, the per-viewer/session
// command lane with its Sync-generation cancel registry, and session route
// resolution. The lanes/generationQueues/aggregateDepth state below MUST stay
// singleton to this module: both control paths serialize against the same lane
// tails, so a second copy would silently unorder viewport against input.

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

/** Run one command behind the per-viewer/session lane. `run` is handed a
 * one-shot `releaseLane`: calling it lets the next queued command start while
 * this one keeps finalizing. Both viewport and input release at worker-send
 * admission, so the socket write order that the worker's keeper-admission lane
 * depends on is still fixed here, but a viewport parked on a keeper resize no
 * longer holds the following PTY input hostage to its own result. Depth counts
 * the command until it truly settles, so the lane stays bounded either way. */
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
  channel: number;
}

export async function resolveSessionRoute(db: KyselyDB, sessionId: string): Promise<SessionRoute | null> {
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
