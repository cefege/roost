// Defines Sync-v2's retained-frame, domain, terminal-lane, and socket state vocabulary.
// The mutable state owner imports these shapes while scheduler and queue modules share them.
// This module has no socket lifecycle behavior or application-frame ownership.

import type { FirehoseFrame, SyncDomain } from "@roost/protocol/proto/sync_pb";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type {
  TerminalSnapshotCursor as TerminalSnapshotPartsCursor,
} from "../terminal/screen/terminal-screen-frames.ts";

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
  /** Every part of one chunked snapshot must retain the same timing metadata. */
  fanoutMs: bigint | null;
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
