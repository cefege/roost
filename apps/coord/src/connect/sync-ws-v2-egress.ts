// Sync v2 weighted-lane egress scheduler.
//
// One queue per domain, each preserving its own snapshot/live boundary, drained
// through a fixed weighted lane rotation (8 cell : 4 session : 2 retained : 1
// nonterminal) with an age escape hatch so a low lane cannot starve. Respects
// the same application ACK window as the v1 send path.
//
// Frame order is load-bearing: we may step past an ineligible fenced cell to
// reach its opened event, but two eligible items from one domain never reorder.

import type { ServerWebSocket } from "bun";
import { clone, create, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomainResetFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncDeadlineClock } from "./sync-ws-deadline.ts";
import {
  APPLICATION_MAX_UNACKED_BYTES,
  APPLICATION_MAX_UNACKED_FRAMES,
  type SyncBackpressureReason,
} from "./sync-ws-v1-delivery.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import { makeSyncV2ControlSender } from "./sync-ws-v2-control.ts";
import {
  V2_AGGREGATE_MAX_QUEUED_BYTES,
  V2_AGGREGATE_MAX_QUEUED_FRAMES,
  V2_DOMAIN_MAX_QUEUED_BYTES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  V2_LOW_LANE_MAX_AGE_MS,
  V2_WEIGHTED_LANES,
  allocateDomainGeneration,
  clearV2DomainQueue,
  isV2SnapshotFrame,
  queuedV2FrameEligible,
  removeQueuedV2Cells,
  type SyncV2DomainState,
  type SyncV2QueuedFrame,
} from "./sync-ws-v2-state.ts";
import { makeSyncV2TerminalScheduler } from "./sync-ws-v2-terminal.ts";

export interface SyncV2SchedulerDeps {
  readonly deadlineClock: SyncDeadlineClock;
  readonly backpressureLimitBytes: number;
  readonly backpressureTimeoutMs: number;
  closeForBackpressure(
    ws: ServerWebSocket<SyncWsData>,
    reason: SyncBackpressureReason,
    frame: string,
  ): void;
  closeForDroppedFrame(
    ws: ServerWebSocket<SyncWsData>,
    frame: string,
    encodedBytes: number,
    bufferedBytes: number,
  ): void;
  rearmApplicationDeadline(ws: ServerWebSocket<SyncWsData>): void;
}

function isTerminalCellFrame(frame: FirehoseFrame): boolean {
  return frame.frame.case === "cellGrid" || frame.frame.case === "cellGridChunk";
}
// A freshly attached session's baseline may pass other sessions' queued deltas,
// but never this session's own queued frames (per-session FIFO, view states
// before chunks) or another session's snapshot (no viewer's baseline waits
// behind a later attach). Returns the queue position to splice into.
function v2AttachSnapshotInsertIndex(
  queue: readonly SyncV2QueuedFrame[],
  sessionId: string | undefined,
): number {
  let insertIndex = queue.length;
  for (let index = queue.length - 1; index >= 0; index -= 1) {
    const item = queue[index]!;
    if (
      (item.meta.sessionId !== undefined && item.meta.sessionId === sessionId)
      || isV2SnapshotFrame(item.frame)
    ) {
      return index + 1;
    }
    insertIndex = index;
  }
  return insertIndex;
}


export function makeSyncV2Scheduler(deps: SyncV2SchedulerDeps) {
  const {
    deadlineClock,
    backpressureLimitBytes,
    backpressureTimeoutMs,
    closeForBackpressure,
    closeForDroppedFrame,
    rearmApplicationDeadline,
  } = deps;
  const sendV2ControlFrame = makeSyncV2ControlSender(deps);

  function resetV2Domain(
    ws: ServerWebSocket<SyncWsData>,
    domainId: SyncDomain,
    reason: string,
  ): void {
    const v2 = ws.data.v2;
    const domain = v2?.domains.get(domainId);
    if (!v2 || !domain || ws.data.pressureClosing) return;
    clearV2DomainQueue(ws, domain);
    domain.generation = allocateDomainGeneration();
    domain.ready = false;
    if (domainId === SyncDomain.TERMINAL) {
      v2.announcedSessions.clear();
      v2.pendingSessionAnnouncements.clear();
      terminalScheduler.clearSessions(ws);
    }
    sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
      frame: {
        case: "domainReset",
        value: create(SyncDomainResetFrameSchema, {
          domain: domainId,
          generation: domain.generation,
          reason,
          subscribed: domain.subscribed,
        }),
      },
    }));
  }

  function selectV2Candidate(
    ws: ServerWebSocket<SyncWsData>,
  ): { domain: SyncV2DomainState; index: number; item: SyncV2QueuedFrame } | null {
    const v2 = ws.data.v2;
    if (!v2) return null;
    type Candidate = { domain: SyncV2DomainState; index: number; item: SyncV2QueuedFrame };
    const heads: Candidate[] = [];
    // Preserve each domain's snapshot/live boundary. We may step past an
    // ineligible fenced cell to reach its opened event, but never reorder two
    // eligible items from the same domain.
    for (const domain of v2.domains.values()) {
      if (!domain.subscribed || !domain.ready) continue;
      for (let index = 0; index < domain.queue.length; index += 1) {
        const item = domain.queue[index]!;
        if (!queuedV2FrameEligible(v2, ws.data.ackDeliverySeq, item)) continue;
        heads.push({ domain, index, item });
        break;
      }
    }
    if (heads.length === 0) return null;

    const now = deadlineClock.now();
    let overdue: Candidate | null = null;
    for (const candidate of heads) {
      if (
        candidate.item.meta.lane !== "cell"
        && now - candidate.item.queuedAtMs >= V2_LOW_LANE_MAX_AGE_MS
        && (!overdue || candidate.item.queuedAtMs < overdue.item.queuedAtMs)
      ) overdue = candidate;
    }
    if (overdue) return overdue;

    for (let attempt = 0; attempt < V2_WEIGHTED_LANES.length; attempt += 1) {
      const lane = V2_WEIGHTED_LANES[v2.laneCursor]!;
      v2.laneCursor = (v2.laneCursor + 1) % V2_WEIGHTED_LANES.length;
      let selected: Candidate | null = null;
      for (const candidate of heads) {
        if (candidate.item.meta.lane !== lane) continue;
        if (!selected || candidate.item.queuedAtMs < selected.item.queuedAtMs) {
          selected = candidate;
        }
      }
      if (selected) return selected;
    }
    return heads.reduce((oldest, candidate) =>
      candidate.item.queuedAtMs < oldest.item.queuedAtMs ? candidate : oldest
    );
  }

  function removeTerminalQueued(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    includeState: boolean,
  ): void {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal) return;
    for (let index = terminal.queue.length - 1; index >= 0; index--) {
      const item = terminal.queue[index]!;
      if (item.meta.sessionId !== sessionId || item.meta.lane !== "cell") continue;
      if (!includeState && !isTerminalCellFrame(item.frame)) continue;
      if (index < terminal.seedInsertIndex) terminal.seedInsertIndex--;
      terminal.queue.splice(index, 1);
      terminal.queuedBytes -= item.estimatedBytes;
      v2.queuedFrames--;
      v2.queuedBytes -= item.estimatedBytes;
    }
  }

  function enqueueV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
    meta?: SyncFeedFrameMeta,
  ): boolean {
    const v2 = ws.data.v2;
    const effectiveMeta = meta ?? { domain: null, lane: "control" as const };
    if (!v2 || ws.data.pressureClosing) return false;
    if (effectiveMeta.domain === null || effectiveMeta.lane === "control") {
      return sendV2ControlFrame(ws, frame);
    }
    const domain = v2.domains.get(effectiveMeta.domain);
    if (!domain || !domain.subscribed) return false;
    const owned = clone(FirehoseFrameSchema, frame);
    owned.deliverySeq = 0n;
    owned.domain = effectiveMeta.domain;
    owned.domainGeneration = domain.generation;
    const estimatedBytes = toBinary(FirehoseFrameSchema, owned).byteLength + 10;
    const exceedsDomain = domain.queue.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES
      || domain.queuedBytes + estimatedBytes > V2_DOMAIN_MAX_QUEUED_BYTES;
    const exceedsAggregate = v2.queuedFrames + 1 > V2_AGGREGATE_MAX_QUEUED_FRAMES
      || v2.queuedBytes + estimatedBytes > V2_AGGREGATE_MAX_QUEUED_BYTES;

    if (exceedsDomain || exceedsAggregate) {
      if (effectiveMeta.domain !== SyncDomain.TERMINAL) {
        resetV2Domain(
          ws,
          effectiveMeta.domain,
          exceedsDomain ? "domain_overflow" : "aggregate_overflow",
        );
      }
      return false;
    }
    const item: SyncV2QueuedFrame = {
      frame: owned,
      meta: effectiveMeta,
      queuedAtMs: deadlineClock.now(),
      estimatedBytes,
    };
    if (effectiveMeta.beforeBuffered) {
      domain.queue.splice(domain.seedInsertIndex, 0, item);
      domain.seedInsertIndex++;
    } else if (effectiveMeta.attachSnapshot) {
      const insertIndex = v2AttachSnapshotInsertIndex(domain.queue, effectiveMeta.sessionId);
      domain.queue.splice(insertIndex, 0, item);
      if (insertIndex < domain.seedInsertIndex) domain.seedInsertIndex++;
    } else {
      domain.queue.push(item);
    }
    domain.queuedBytes += estimatedBytes;
    v2.queuedFrames++;
    v2.queuedBytes += estimatedBytes;
    scheduleV2(ws);
    return true;
  }
  const terminalScheduler = makeSyncV2TerminalScheduler({
    deadlineClock,
    enqueueV2Frame,
    removeTerminalQueued,
  });
  let scheduleV2: (ws: ServerWebSocket<SyncWsData>) => void;
  const scheduleV2Yield = (
    ws: ServerWebSocket<SyncWsData>,
    v2: NonNullable<SyncWsData["v2"]>,
  ): void => {
    if (
      ws.data.v2 !== v2
      || ws.data.pressureClosing
      || v2.schedulerYieldTimer !== null
    ) return;
    let timer: Timer | null = null;
    timer = deadlineClock.setTimeout(() => {
      // A close/reset may have cleared this handle while the callback was
      // already queued by the deadline clock. Only the current continuation
      // may reopen the normal microtask scheduler.
      if (v2.schedulerYieldTimer !== timer) return;
      v2.schedulerYieldTimer = null;
      if (ws.data.v2 !== v2 || ws.data.pressureClosing) return;
      scheduleV2(ws);
    }, 0);
    v2.schedulerYieldTimer = timer;
  };
  const flushV2 = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2 || ws.data.pressureClosing) return;
    v2.schedulerPending = false;
    for (let sentCount = 0; sentCount < 64; sentCount++) {
      const candidate = selectV2Candidate(ws);
      if (!candidate) return;
      const nextSeq = ws.data.lastSentDeliverySeq + 1n;
      const outbound = clone(FirehoseFrameSchema, candidate.item.frame);
      outbound.deliverySeq = nextSeq;
      const binary = toBinary(FirehoseFrameSchema, outbound);
      if (
        ws.data.deliveryQueue.length >= APPLICATION_MAX_UNACKED_FRAMES
        || ws.data.unackedEncodedBytes + binary.byteLength > APPLICATION_MAX_UNACKED_BYTES
      ) return;

      const sentAtMs = deadlineClock.now();
      const frameKind = outbound.frame.case ?? "application";
      let result: number;
      let bufferedBytes = 0;
      try {
        result = ws.send(binary);
        bufferedBytes = ws.getBufferedAmount();
      } catch {
        // A throwing send may already have handed the frame to the socket. Do
        // not acknowledge or retry that ambiguous delivery on this connection;
        // closing forces the replacement socket to establish a fresh baseline.
        closeForDroppedFrame(ws, frameKind, binary.byteLength, bufferedBytes);
        return;
      }
      if (result === 0) {
        closeForDroppedFrame(ws, frameKind, binary.byteLength, bufferedBytes);
        return;
      }
      candidate.domain.queue.splice(candidate.index, 1);
      if (candidate.index < candidate.domain.seedInsertIndex) {
        candidate.domain.seedInsertIndex--;
      }
      candidate.domain.queuedBytes -= candidate.item.estimatedBytes;
      v2.queuedFrames--;
      v2.queuedBytes -= candidate.item.estimatedBytes;
      terminalScheduler.pumpSessions(ws);
      ws.data.lastSentDeliverySeq = nextSeq;
      ws.data.unackedEncodedBytes += binary.byteLength;
      ws.data.deliveryQueue.push({
        seq: nextSeq,
        encodedBytes: binary.byteLength,
        sentAtMs,
      });
      if (!ws.data.deliveryTimer) rearmApplicationDeadline(ws);

      if (candidate.item.meta.announces) {
        for (const sessionId of candidate.item.meta.announces) {
          v2.announcedSessions.add(sessionId);
          v2.pendingSessionAnnouncements.set(sessionId, nextSeq);
        }
      }
      if (candidate.item.meta.closes) {
        const closed = new Set(candidate.item.meta.closes);
        for (const sessionId of closed) {
          v2.announcedSessions.delete(sessionId);
          v2.pendingSessionAnnouncements.delete(sessionId);
          terminalScheduler.deleteSession(ws, sessionId);
        }
        removeQueuedV2Cells(ws, closed);
      }

      terminalScheduler.onFrameDelivered(ws, candidate.item.meta);

      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", frameKind);
        return;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = frameKind;
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? frameKind);
        }, backpressureTimeoutMs);
      }
    }
    const laneCursorBeforeProbe = v2.laneCursor;
    const hasCandidate = selectV2Candidate(ws) !== null;
    v2.laneCursor = laneCursorBeforeProbe;
    if (hasCandidate) scheduleV2Yield(ws, v2);
  };

  scheduleV2 = (ws): void => {
    const v2 = ws.data.v2;
    if (
      !v2
      || v2.schedulerPending
      || v2.schedulerYieldTimer !== null
      || ws.data.pressureClosing
    ) return;
    v2.schedulerPending = true;
    queueMicrotask(() => {
      if (v2.schedulerYieldTimer !== null) {
        v2.schedulerPending = false;
        return;
      }
      if (ws.data.v2 === v2 && v2.schedulerPending && !ws.data.pressureClosing) {
        flushV2(ws);
      }
    });
  };

  return {
    sendV2ControlFrame,
    resetV2Domain,
    scheduleV2,
    enqueueV2Frame,
    beginTerminalStream: terminalScheduler.beginTerminalStream,
    enqueueTerminalState: terminalScheduler.enqueueTerminalState,
    replaceTerminalSnapshot: terminalScheduler.replaceTerminalSnapshot,
    enqueueTerminalDelta: terminalScheduler.enqueueTerminalDelta,
    dropTerminalSession: terminalScheduler.dropTerminalSession,
  };
}

export type SyncV2Scheduler = ReturnType<typeof makeSyncV2Scheduler>;
