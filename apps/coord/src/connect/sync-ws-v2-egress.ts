// Coordinates Sync v2 domain reset, queue admission, progress deadlines, and
// bounded writes through the weighted-lane selector. Each domain preserves its
// snapshot/live boundary, while terminal work without an ACK owner gets a
// separate deadline so a fenced cell queue cannot stall indefinitely.

import type { ServerWebSocket } from "bun";
import { clone, create, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomainResetFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { WsDeadlineClock } from "./ws-auth-deadline.ts";
import {
  APPLICATION_MAX_UNACKED_BYTES,
  APPLICATION_ACK_TIMEOUT_MS,
  APPLICATION_MAX_UNACKED_FRAMES,
  type SyncBackpressureReason,
} from "./sync-ws-v1-delivery.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import { makeSyncV2ControlSender } from "./sync-ws-v2-control.ts";
import {
  V2_DOMAIN_MAX_QUEUED_BYTES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  allocateDomainGeneration,
  clearV2DomainQueue,
  ownV2ApplicationFrame,
  queuedV2FrameEligible,
  releaseV2AggregateFrame,
  removeQueuedV2Cells,
  tryRetainV2AggregateFrame,
  type SyncV2OwnedFrame,
  type SyncV2QueuedFrame,
  type SyncV2RetainedFrame,
} from "./sync-ws-v2-state.ts";
import { makeSyncV2TerminalScheduler } from "./sync-ws-v2-terminal.ts";
import { removeTerminalQueued, selectV2Candidate, v2AttachSnapshotInsertIndex } from "./sync-ws-v2-queue.ts";

export interface SyncV2SchedulerDeps {
  readonly deadlineClock: WsDeadlineClock;
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
    if (domainId === SyncDomain.TERMINAL && v2.terminalProgressTimer !== null) {
      deadlineClock.clearTimeout(v2.terminalProgressTimer);
      v2.terminalProgressTimer = null;
    }
    clearV2DomainQueue(ws, domain);
    domain.generation = allocateDomainGeneration();
    domain.ready = false;
    if (domainId === SyncDomain.TERMINAL) {
      v2.announcedSessions.clear();
      v2.pendingSessionAnnouncements.clear();
      terminalScheduler.clearSessions(ws);
    }
    const sent = sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
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
    if (!sent && !ws.data.pressureClosing) {
      closeForDroppedFrame(ws, "domainReset", 0, 0);
    }
  }

  function oldestTerminalWorkWithoutAckOwner(ws: ServerWebSocket<SyncWsData>): number | null {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal || terminal.queue.length === 0) return null;
    let oldest: number | null = null;
    for (const item of terminal.queue) {
      let ownsQueuedDeadline = !terminal.ready;
      if (terminal.ready && !queuedV2FrameEligible(v2, ws.data.ackDeliverySeq, item)) {
        const sessionId = item.meta.sessionId;
        const announcementSeq = sessionId === undefined
          ? undefined
          : v2.pendingSessionAnnouncements.get(sessionId);
        ownsQueuedDeadline = announcementSeq === undefined
          || ws.data.ackDeliverySeq >= announcementSeq;
      }
      if (!ownsQueuedDeadline) continue;
      if (oldest === null || item.queuedAtMs < oldest) oldest = item.queuedAtMs;
    }
    return oldest;
  }

  function refreshTerminalProgressDeadline(ws: ServerWebSocket<SyncWsData>): void {
    const v2 = ws.data.v2;
    if (!v2 || ws.data.pressureClosing) return;
    const oldest = oldestTerminalWorkWithoutAckOwner(ws);
    if (oldest === null) {
      if (v2.terminalProgressTimer !== null) {
        deadlineClock.clearTimeout(v2.terminalProgressTimer);
        v2.terminalProgressTimer = null;
      }
      return;
    }
    if (v2.terminalProgressTimer !== null) return;
    let timer: Timer | null = null;
    const onDeadline = (): void => {
      if (v2.terminalProgressTimer !== timer) return;
      v2.terminalProgressTimer = null;
      if (ws.data.v2 !== v2 || ws.data.pressureClosing) return;
      const currentOldest = oldestTerminalWorkWithoutAckOwner(ws);
      if (currentOldest === null) return;
      const remaining = currentOldest + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now();
      if (remaining > 0) {
        refreshTerminalProgressDeadline(ws);
        return;
      }
      resetV2Domain(ws, SyncDomain.TERMINAL, "queued_progress_timeout");
    };
    timer = deadlineClock.setTimeout(
      onDeadline,
      Math.max(0, oldest + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now()),
    );
    v2.terminalProgressTimer = timer;
  }

  function enqueuePreparedV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    owned: SyncV2OwnedFrame,
    meta: SyncFeedFrameMeta,
    retained?: SyncV2RetainedFrame,
  ): boolean {
    const v2 = ws.data.v2;
    if (!v2 || ws.data.pressureClosing || meta.domain === null || meta.lane === "control") {
      return false;
    }
    const domain = v2.domains.get(meta.domain);
    if (!domain || !domain.subscribed) return false;
    const exceedsDomain = domain.queue.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES
      || domain.queuedBytes + owned.estimatedBytes > V2_DOMAIN_MAX_QUEUED_BYTES;
    if (exceedsDomain) {
      resetV2Domain(ws, meta.domain, "domain_overflow");
      return false;
    }
    const aggregateOwner = retained ?? tryRetainV2AggregateFrame(v2, owned);
    if (!aggregateOwner) {
      resetV2Domain(ws, meta.domain, "aggregate_overflow");
      return false;
    }
    const item: SyncV2QueuedFrame = {
      ...aggregateOwner,
      meta,
      queuedAtMs: deadlineClock.now(),
    };
    if (meta.beforeBuffered) {
      domain.queue.splice(domain.seedInsertIndex, 0, item);
      domain.seedInsertIndex++;
    } else if (meta.attachSnapshot) {
      const insertIndex = v2AttachSnapshotInsertIndex(domain.queue, meta.sessionId);
      domain.queue.splice(insertIndex, 0, item);
      if (insertIndex < domain.seedInsertIndex) domain.seedInsertIndex++;
    } else {
      domain.queue.push(item);
    }
    domain.queuedBytes += owned.estimatedBytes;
    scheduleV2(ws);
    if (meta.domain === SyncDomain.TERMINAL) refreshTerminalProgressDeadline(ws);
    return true;
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
    return enqueuePreparedV2Frame(
      ws,
      ownV2ApplicationFrame(frame, effectiveMeta.domain, domain.generation),
      effectiveMeta,
    );
  }

  function enqueueRetainedV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    retained: SyncV2RetainedFrame,
    meta: SyncFeedFrameMeta,
  ): boolean {
    const v2 = ws.data.v2;
    const domain = meta.domain === null ? undefined : v2?.domains.get(meta.domain);
    if (
      !v2
      || !domain
      || !retained.aggregateCharge.retained
      || retained.frame.domain !== meta.domain
      || retained.frame.domainGeneration !== domain.generation
    ) return false;
    return enqueuePreparedV2Frame(ws, retained, meta, retained);
  }

  const terminalScheduler = makeSyncV2TerminalScheduler({
    deadlineClock,
    enqueueRetainedV2Frame,
    removeTerminalQueued,
    onTerminalOverflow: (ws, reason) =>
      resetV2Domain(ws, SyncDomain.TERMINAL, reason),
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
      const candidate = selectV2Candidate(ws, deadlineClock);
      if (!candidate) {
        refreshTerminalProgressDeadline(ws);
        return;
      }
      const nextSeq = ws.data.lastSentDeliverySeq + 1n;
      const outbound = clone(FirehoseFrameSchema, candidate.item.frame);
      outbound.deliverySeq = nextSeq;
      const binary = toBinary(FirehoseFrameSchema, outbound);
      if (
        ws.data.deliveryQueue.length >= APPLICATION_MAX_UNACKED_FRAMES
        || ws.data.unackedEncodedBytes + binary.byteLength > APPLICATION_MAX_UNACKED_BYTES
      ) {
        refreshTerminalProgressDeadline(ws);
        return;
      }

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
      releaseV2AggregateFrame(v2, candidate.item);
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
      refreshTerminalProgressDeadline(ws);

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
    refreshTerminalProgressDeadline(ws);
    const laneCursorBeforeProbe = v2.laneCursor;
    const hasCandidate = selectV2Candidate(ws, deadlineClock) !== null;
    v2.laneCursor = laneCursorBeforeProbe;
    if (hasCandidate) scheduleV2Yield(ws, v2);
  };

  scheduleV2 = (ws): void => {
    const v2 = ws.data.v2;
    refreshTerminalProgressDeadline(ws);
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
