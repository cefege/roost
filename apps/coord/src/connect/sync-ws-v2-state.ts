// Owns Sync-v2 per-socket domain state, weighted lanes, queue limits, and mutators
// shared by the scheduler and client-command ingress. Its process-wide generation
// allocator is intentionally monotonic so two generations inside one process epoch
// cannot collide; all other mutable state remains owned by the individual socket.

import type { ServerWebSocket } from "bun";
import { randomUUID } from "node:crypto";
import { clone, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta, SyncFeedLane } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import type {
  TerminalSnapshotCursor as TerminalSnapshotPartsCursor,
} from "./terminal-screen-frames.ts";

export const V2_DOMAIN_MAX_QUEUED_FRAMES = 512;
export const V2_DOMAIN_MAX_QUEUED_BYTES = 4 * 1024 * 1024;
export const V2_TERMINAL_MAX_RETAINED_FRAMES = 512;
export const V2_TERMINAL_MAX_RETAINED_BYTES = 4 * 1024 * 1024;
export const V2_NONTERMINAL_MAX_RETAINED_FRAMES = 512;
export const V2_NONTERMINAL_MAX_RETAINED_BYTES = 4 * 1024 * 1024;
export const V2_AGGREGATE_MAX_QUEUED_FRAMES =
  V2_TERMINAL_MAX_RETAINED_FRAMES + V2_NONTERMINAL_MAX_RETAINED_FRAMES;
export const V2_AGGREGATE_MAX_QUEUED_BYTES =
  V2_TERMINAL_MAX_RETAINED_BYTES + V2_NONTERMINAL_MAX_RETAINED_BYTES;
export const V2_TERMINAL_LANE_MAX_DELTA_FRAMES = 32;
export const V2_TERMINAL_LANE_MAX_DELTA_BYTES = 4 * 1024 * 1024;
/** Cell material leaves this charged terminal credit for state/feed frames. */
export const V2_TERMINAL_RELIABLE_RESERVE_FRAMES = 64;
export const V2_TERMINAL_RELIABLE_RESERVE_BYTES = 256 * 1024;
export const V2_TERMINAL_CELL_MAX_RETAINED_FRAMES =
  V2_TERMINAL_MAX_RETAINED_FRAMES - V2_TERMINAL_RELIABLE_RESERVE_FRAMES;
export const V2_TERMINAL_CELL_MAX_RETAINED_BYTES =
  V2_TERMINAL_MAX_RETAINED_BYTES - V2_TERMINAL_RELIABLE_RESERVE_BYTES;
export const V2_LOW_LANE_MAX_AGE_MS = 100;
export const V2_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
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
  return domain === SyncDomain.AUDIT;
}

export function isV2SnapshotFrame(frame: FirehoseFrame): boolean {
  return frame.frame.case === "cellGridChunk"
    || (frame.frame.case === "cellGrid" && frame.frame.value.full);
}

function isTerminalCellMaterial(frame: FirehoseFrame): boolean {
  return frame.frame.case === "cellGrid" || frame.frame.case === "cellGridChunk";
}

export interface SyncV2OwnedFrame {
  readonly frame: FirehoseFrame;
  readonly estimatedBytes: number;
}

export interface SyncV2AggregateCharge {
  readonly estimatedBytes: number;
  readonly terminal: boolean;
  readonly terminalCell: boolean;
  retained: boolean;
}

export interface SyncV2RetainedFrame extends SyncV2OwnedFrame {
  readonly aggregateCharge: SyncV2AggregateCharge;
}
export interface SyncTerminalDeltaFrame extends SyncV2RetainedFrame {
  readonly payloadBytes: number;
}


export interface SyncV2QueuedFrame extends SyncV2RetainedFrame {
  readonly meta: SyncFeedFrameMeta;
  readonly queuedAtMs: number;
}

export interface SyncTerminalSnapshotCursor {
  readonly streamId: string;
  source: TerminalSnapshotPartsCursor | null;
  index: number;
  queued: boolean;
  /** The one source part currently charged to terminal materialization. */
  materialized: SyncV2RetainedFrame | null;
  readonly deltaTail: SyncTerminalDeltaFrame[];
  deltaBytes: number;
}

export interface SyncTerminalSessionLane {
  streamId: string;
  cursor: SyncTerminalSnapshotCursor | null;
  /** Terminal view-states awaiting their per-session FIFO turn. */
  readonly pendingStates: SyncV2RetainedFrame[];
  stateQueued: boolean;
  /** Set while this lane is present in the socket's deduplicated ready ring. */
  ready: boolean;
  /** A scoped canonical full is needed after the current cursor can release. */
  rebaselinePending: boolean;
  /** The next baseline part may pass foreign deltas once for a new stream. */
  attachPriorityPending: boolean;
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
  readonly terminalSessions: Map<string, SyncTerminalSessionLane>;
  /** Insertion-ordered, deduplicated terminal lanes with an eligible head. */
  readonly terminalReadySessions: Set<string>;
  /** Terminal's half of retained application materialization. */
  terminalRetainedFrames: number;
  terminalRetainedBytes: number;
  /** Charged cell payloads, capped below terminal's reliable semantic reserve. */
  terminalCellRetainedFrames: number;
  terminalCellRetainedBytes: number;
  /** Aggregate payload ownership across domain queues and terminal auxiliaries. */
  queuedFrames: number;
  queuedBytes: number;
  laneCursor: number;
  schedulerPending: boolean;
  schedulerYieldTimer: Timer | null;
  snapshotDispose: (() => void) | null;
  layoutTargetDispose: (() => void) | null;
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
    terminalSessions: new Map(),
    terminalReadySessions: new Set(),
    terminalRetainedFrames: 0,
    terminalRetainedBytes: 0,
    terminalCellRetainedFrames: 0,
    terminalCellRetainedBytes: 0,
    queuedFrames: 0,
    queuedBytes: 0,
    laneCursor: 0,
    schedulerPending: false,
    schedulerYieldTimer: null,
    snapshotDispose: null,
    layoutTargetDispose: null,
    closeNotified: false,
  };
}
export function ownV2ApplicationFrame(
  frame: FirehoseFrame,
  domain: SyncDomain,
  generation: bigint,
): SyncV2OwnedFrame {
  const owned = clone(FirehoseFrameSchema, frame);
  owned.deliverySeq = 0n;
  owned.domain = domain;
  owned.domainGeneration = generation;
  return {
    frame: owned,
    estimatedBytes: toBinary(FirehoseFrameSchema, owned).byteLength + 10,
  };
}

export function tryRetainV2AggregateFrame(
  v2: SyncV2SocketState,
  owned: SyncV2OwnedFrame,
): SyncV2RetainedFrame | null {
  const terminal = owned.frame.domain === SyncDomain.TERMINAL;
  const terminalCell = terminal && isTerminalCellMaterial(owned.frame);
  const retainedFrames = terminal
    ? v2.terminalRetainedFrames
    : v2.queuedFrames - v2.terminalRetainedFrames;
  const retainedBytes = terminal
    ? v2.terminalRetainedBytes
    : v2.queuedBytes - v2.terminalRetainedBytes;
  const frameLimit = terminal
    ? V2_TERMINAL_MAX_RETAINED_FRAMES
    : V2_NONTERMINAL_MAX_RETAINED_FRAMES;
  const byteLimit = terminal
    ? V2_TERMINAL_MAX_RETAINED_BYTES
    : V2_NONTERMINAL_MAX_RETAINED_BYTES;
  if (
    retainedFrames + 1 > frameLimit
    || retainedBytes + owned.estimatedBytes > byteLimit
    || (terminalCell && (
      v2.terminalCellRetainedFrames + 1 > V2_TERMINAL_CELL_MAX_RETAINED_FRAMES
      || v2.terminalCellRetainedBytes + owned.estimatedBytes > V2_TERMINAL_CELL_MAX_RETAINED_BYTES
    ))
    || v2.queuedFrames + 1 > V2_AGGREGATE_MAX_QUEUED_FRAMES
    || v2.queuedBytes + owned.estimatedBytes > V2_AGGREGATE_MAX_QUEUED_BYTES
  ) return null;
  const aggregateCharge: SyncV2AggregateCharge = {
    estimatedBytes: owned.estimatedBytes,
    terminal,
    terminalCell,
    retained: true,
  };
  v2.queuedFrames++;
  v2.queuedBytes += owned.estimatedBytes;
  if (terminal) {
    v2.terminalRetainedFrames++;
    v2.terminalRetainedBytes += owned.estimatedBytes;
  }
  if (terminalCell) {
    v2.terminalCellRetainedFrames++;
    v2.terminalCellRetainedBytes += owned.estimatedBytes;
  }
  return { ...owned, aggregateCharge };
}

export function releaseV2AggregateFrame(
  v2: SyncV2SocketState,
  retained: Pick<SyncV2RetainedFrame, "aggregateCharge">,
): void {
  const charge = retained.aggregateCharge;
  if (!charge.retained) return;
  charge.retained = false;
  v2.queuedFrames--;
  v2.queuedBytes -= charge.estimatedBytes;
  if (charge.terminal) {
    v2.terminalRetainedFrames--;
    v2.terminalRetainedBytes -= charge.estimatedBytes;
  }
  if (charge.terminalCell) {
    v2.terminalCellRetainedFrames--;
    v2.terminalCellRetainedBytes -= charge.estimatedBytes;
  }
}

export function releaseV2TerminalDeltaTail(
  v2: SyncV2SocketState,
  cursor: SyncTerminalSnapshotCursor,
): void {
  for (const delta of cursor.deltaTail) releaseV2AggregateFrame(v2, delta);
  cursor.deltaTail.length = 0;
  cursor.deltaBytes = 0;
}

export function releaseV2TerminalCursor(
  v2: SyncV2SocketState,
  lane: SyncTerminalSessionLane,
): void {
  const cursor = lane.cursor;
  if (!cursor) return;
  if (cursor.materialized) releaseV2AggregateFrame(v2, cursor.materialized);
  cursor.materialized = null;
  cursor.source?.release();
  releaseV2TerminalDeltaTail(v2, cursor);
  lane.cursor = null;
}

export function releaseV2TerminalLane(
  v2: SyncV2SocketState,
  lane: SyncTerminalSessionLane,
): void {
  for (const state of lane.pendingStates) releaseV2AggregateFrame(v2, state);
  lane.pendingStates.length = 0;
  lane.stateQueued = false;
  lane.ready = false;
  lane.rebaselinePending = false;
  releaseV2TerminalCursor(v2, lane);
}


export const clearV2State = (
  ws: ServerWebSocket<SyncWsData>,
  clearTimer: (timer: Timer) => void = (timer) => clearTimeout(timer),
): void => {
  const v2 = ws.data.v2;
  if (!v2) return;
  v2.schedulerPending = false;
  if (v2.schedulerYieldTimer !== null) {
    clearTimer(v2.schedulerYieldTimer);
    v2.schedulerYieldTimer = null;
  }
  for (const domain of v2.domains.values()) {
    for (const item of domain.queue) releaseV2AggregateFrame(v2, item);
    domain.queue.length = 0;
    domain.queuedBytes = 0;
    domain.seedInsertIndex = 0;
    domain.ready = false;
  }
  for (const lane of v2.terminalSessions.values()) {
    releaseV2TerminalLane(v2, lane);
  }
  v2.queuedFrames = 0;
  v2.queuedBytes = 0;
  v2.terminalRetainedFrames = 0;
  v2.terminalRetainedBytes = 0;
  v2.terminalCellRetainedFrames = 0;
  v2.terminalCellRetainedBytes = 0;
  v2.announcedSessions.clear();
  v2.pendingSessionAnnouncements.clear();
  v2.terminalReadySessions.clear();
  v2.terminalSessions.clear();
  v2.snapshotDispose?.();
  v2.layoutTargetDispose?.();
  v2.layoutTargetDispose = null;
  v2.snapshotDispose = null;
};


export const clearV2DomainQueue = (
  ws: ServerWebSocket<SyncWsData>,
  domain: SyncV2DomainState,
): void => {
  const v2 = ws.data.v2;
  if (v2) {
    for (const item of domain.queue) releaseV2AggregateFrame(v2, item);
  }
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
    releaseV2AggregateFrame(v2, item);
  }
};

export const queuedV2FrameEligible = (
  v2: SyncV2SocketState,
  ackDeliverySeq: bigint,
  item: SyncV2QueuedFrame,
): boolean => {
  if (item.meta.lane !== "cell") return true;
  if (item.frame.frame.case === "terminalViewState") return true;
  if (!item.meta.sessionId || !v2.announcedSessions.has(item.meta.sessionId)) return false;
  const announcementSeq = v2.pendingSessionAnnouncements.get(item.meta.sessionId);
  return announcementSeq === undefined || ackDeliverySeq >= announcementSeq;
};
