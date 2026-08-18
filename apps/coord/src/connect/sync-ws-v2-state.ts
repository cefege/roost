// Sync v2 per-socket state vocabulary: the domain-generation allocator, the
// weighted lane table, the queue-limit constants, and the pure queue mutators
// that the v2 scheduler and the v2 command handler both operate on.
//
// Split out of sync-ws-handler.ts. The generation counter is module-level on
// purpose: one coord process, one monotonic sequence, so two generations issued
// inside a single process epoch can never collide.

import type { ServerWebSocket } from "bun";
import { randomUUID } from "node:crypto";
import { SyncDomain, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta, SyncFeedLane } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

export const V2_DOMAIN_MAX_QUEUED_FRAMES = 512;
export const V2_DOMAIN_MAX_QUEUED_BYTES = 4 * 1024 * 1024;
export const V2_AGGREGATE_MAX_QUEUED_FRAMES = 1_024;
export const V2_AGGREGATE_MAX_QUEUED_BYTES = 8 * 1024 * 1024;
export const V2_LOW_LANE_MAX_AGE_MS = 100;
export const V2_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.PERMISSIONS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.WEBHOOK,
  SyncDomain.AUDIT,
] as const;
export const V2_WEIGHTED_LANES: readonly SyncFeedLane[] = [
  "cell", "cell", "cell", "cell", "cell", "cell", "cell", "cell",
  "session", "session", "session", "session",
  "retained", "retained",
  "nonterminal",
];
let nextDomainGeneration = BigInt(Date.now()) * 1024n;

export function allocateDomainGeneration(): bigint {
  nextDomainGeneration += 1n;
  return nextDomainGeneration;
}

export function isLazyDomain(domain: SyncDomain): boolean {
  return domain === SyncDomain.WEBHOOK || domain === SyncDomain.AUDIT;
}

export interface SyncV2QueuedFrame {
  readonly frame: FirehoseFrame;
  readonly meta: SyncFeedFrameMeta;
  readonly queuedAtMs: number;
  readonly estimatedBytes: number;
}

export interface SyncV2DomainState {
  generation: bigint;
  subscribed: boolean;
  ready: boolean;
  queue: SyncV2QueuedFrame[];
  queuedBytes: number;
  /** Next insertion point for the retained snapshot preceding buffered live frames. */
  seedInsertIndex: number;
}

export interface SyncV2SocketState {
  readonly socketId: string;
  readonly domains: Map<SyncDomain, SyncV2DomainState>;
  readonly announcedSessions: Set<string>;
  readonly pendingSessionAnnouncements: Map<string, bigint>;
  queuedFrames: number;
  queuedBytes: number;
  laneCursor: number;
  schedulerPending: boolean;
  snapshotDispose: (() => void) | null;
  closeNotified: boolean;
}

export function createSyncV2SocketState(): SyncV2SocketState {
  const domains = new Map<SyncDomain, SyncV2DomainState>();
  for (const domain of V2_DOMAINS) {
    domains.set(domain, {
      generation: allocateDomainGeneration(),
      subscribed: !isLazyDomain(domain),
      ready: false,
      queue: [],
      queuedBytes: 0,
      seedInsertIndex: 0,
    });
  }
  return {
    socketId: randomUUID(),
    domains,
    announcedSessions: new Set(),
    pendingSessionAnnouncements: new Map(),
    queuedFrames: 0,
    queuedBytes: 0,
    laneCursor: 0,
    schedulerPending: false,
    snapshotDispose: null,
    closeNotified: false,
  };
}

export const clearV2State = (ws: ServerWebSocket<SyncWsData>): void => {
  const v2 = ws.data.v2;
  if (!v2) return;
  v2.schedulerPending = false;
  v2.queuedFrames = 0;
  v2.queuedBytes = 0;
  v2.announcedSessions.clear();
  v2.pendingSessionAnnouncements.clear();
  for (const domain of v2.domains.values()) {
    domain.queue.length = 0;
    domain.queuedBytes = 0;
    domain.seedInsertIndex = 0;
    domain.ready = false;
  }
  v2.snapshotDispose?.();
  v2.snapshotDispose = null;
};

export const clearV2DomainQueue = (
  ws: ServerWebSocket<SyncWsData>,
  domain: SyncV2DomainState,
): void => {
  const v2 = ws.data.v2;
  if (!v2 || domain.queue.length === 0) {
    domain.queue.length = 0;
    domain.queuedBytes = 0;
    domain.seedInsertIndex = 0;
    return;
  }
  v2.queuedFrames -= domain.queue.length;
  v2.queuedBytes -= domain.queuedBytes;
  domain.queue.length = 0;
  domain.queuedBytes = 0;
  domain.seedInsertIndex = 0;
};

export const removeQueuedV2Cells = (
  ws: ServerWebSocket<SyncWsData>,
  sessionIds: ReadonlySet<string>,
): void => {
  const v2 = ws.data.v2;
  const terminal = v2?.domains.get(SyncDomain.TERMINAL);
  if (!v2 || !terminal || sessionIds.size === 0) return;
  for (let index = terminal.queue.length - 1; index >= 0; index -= 1) {
    const item = terminal.queue[index]!;
    if (item.meta.lane !== "cell" || !item.meta.sessionId || !sessionIds.has(item.meta.sessionId)) continue;
    if (index < terminal.seedInsertIndex) terminal.seedInsertIndex -= 1;
    terminal.queue.splice(index, 1);
    terminal.queuedBytes -= item.estimatedBytes;
    v2.queuedFrames -= 1;
    v2.queuedBytes -= item.estimatedBytes;
  }
};

export const queuedV2FrameEligible = (
  v2: SyncV2SocketState,
  ackDeliverySeq: bigint,
  item: SyncV2QueuedFrame,
): boolean => {
  if (item.meta.lane !== "cell") return true;
  if (!item.meta.sessionId || !v2.announcedSessions.has(item.meta.sessionId)) return false;
  const announcementSeq = v2.pendingSessionAnnouncements.get(item.meta.sessionId);
  return announcementSeq === undefined || ackDeliverySeq >= announcementSeq;
};
