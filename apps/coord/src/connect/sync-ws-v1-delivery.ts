// Sync WebSocket delivery + backpressure engine.
//
// Owns the cumulative delivery-sequence ACK window that bounds an `flow=1`
// socket at 512 frames / 4 MiB / 3 seconds from the oldest unacknowledged
// send, independently of Bun's native send buffer, plus every close path that
// window can take (1013 backpressure, 1008 invalid ack). sendGuarded and
// pushPacedSeed are the Sync v1 send path; the window bookkeeping, the
// oldest-age deadline and the close paths are SHARED with the v2 weighted-lane
// scheduler, which respects the same limits.
//
// Split out of sync-ws-handler.ts. Nothing here reaches back into the handler
// except the injected cleanupSocket, so every close path still releases native
// timers, the ACK window and the v2 queues together in one place.

import type { ServerWebSocket } from "bun";
import { create, toBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import type { SyncDeadlineClock } from "./sync-ws-deadline.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

export const APPLICATION_MAX_UNACKED_FRAMES = 512;
export const APPLICATION_MAX_UNACKED_BYTES = 4 * 1024 * 1024;
export const APPLICATION_ACK_TIMEOUT_MS = 3_000;

export type SyncBackpressureReason =
  | "high_water"
  | "timeout"
  | "frame_limit"
  | "byte_limit"
  | "age_limit";

export interface SyncV1DeliveryDeps {
  readonly deadlineClock: SyncDeadlineClock;
  readonly backpressureLimitBytes: number;
  readonly backpressureTimeoutMs: number;
  /** Socket teardown, owned by sync-ws-handler: every close path routes through
   *  it so the keepalive, the reauth deadline, native pressure, the ACK window
   *  and the v2 queues are released together. */
  cleanupSocket(ws: ServerWebSocket<SyncWsData>): void;
  /** Sync v2 only: an accepted cumulative ACK reopens the weighted-lane window. */
  scheduleV2(ws: ServerWebSocket<SyncWsData>): void;
}

export function makeSyncV1Delivery(deps: SyncV1DeliveryDeps) {
  const {
    deadlineClock,
    backpressureLimitBytes,
    backpressureTimeoutMs,
    cleanupSocket,
    scheduleV2,
  } = deps;

  const clearNativePressure = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.pressureTimer) {
      deadlineClock.clearTimeout(ws.data.pressureTimer);
      ws.data.pressureTimer = null;
    }
    ws.data.pressureFrame = null;
  };

  const wakeDeliveryWaiters = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryWaiters.size === 0) return;
    const waiters = [...ws.data.deliveryWaiters];
    ws.data.deliveryWaiters.clear();
    for (const wake of waiters) wake();
  };

  const clearApplicationWindow = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryTimer) {
      deadlineClock.clearTimeout(ws.data.deliveryTimer);
      ws.data.deliveryTimer = null;
    }
    ws.data.deliveryQueue.length = 0;
    ws.data.unackedEncodedBytes = 0;
    ws.data.lastSentDeliverySeq = 0n;
    ws.data.ackDeliverySeq = 0n;
    wakeDeliveryWaiters(ws);
  };

  const applicationStats = (ws: ServerWebSocket<SyncWsData>): {
    unackedFrames: number;
    unackedBytes: number;
    oldestAgeMs: number;
  } => {
    const oldest = ws.data.deliveryQueue[0];
    return {
      unackedFrames: ws.data.deliveryQueue.length,
      unackedBytes: ws.data.unackedEncodedBytes,
      oldestAgeMs: oldest ? Math.max(0, deadlineClock.now() - oldest.sentAtMs) : 0,
    };
  };

  function closeForBackpressure(
    ws: ServerWebSocket<SyncWsData>,
    reason: SyncBackpressureReason,
    frame: string,
  ): void {
    if (ws.data.pressureClosing) return;
    const stats = applicationStats(ws);
    let bufferedBytes = 0;
    try { bufferedBytes = ws.getBufferedAmount(); } catch { /* best-effort diagnostic */ }
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    signal("sync.queue_overflow", {
      caller_fp: ws.data.caller.fingerprint,
      reason,
      frame,
      buffered_bytes: bufferedBytes,
      unacked_frames: stats.unackedFrames,
      unacked_bytes: stats.unackedBytes,
      oldest_age_ms: stats.oldestAgeMs,
      cooldownKey: ws.data.caller.fingerprint,
    });
    ws.close(1013, "sync backpressure");
  }

  const rearmApplicationDeadline = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.deliveryTimer) {
      deadlineClock.clearTimeout(ws.data.deliveryTimer);
      ws.data.deliveryTimer = null;
    }
    const oldest = ws.data.deliveryQueue[0];
    if (!oldest || ws.data.pressureClosing) return;

    const onDeadline = (): void => {
      ws.data.deliveryTimer = null;
      if (ws.data.pressureClosing) return;
      const currentOldest = ws.data.deliveryQueue[0];
      if (!currentOldest) return;
      const remainingMs = currentOldest.sentAtMs + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now();
      if (remainingMs > 0) {
        ws.data.deliveryTimer = deadlineClock.setTimeout(onDeadline, remainingMs);
        return;
      }
      closeForBackpressure(ws, "age_limit", "age_deadline");
    };
    ws.data.deliveryTimer = deadlineClock.setTimeout(
      onDeadline,
      Math.max(0, oldest.sentAtMs + APPLICATION_ACK_TIMEOUT_MS - deadlineClock.now()),
    );
  };

  const closeForDroppedFrame = (
    ws: ServerWebSocket<SyncWsData>,
    frame: string,
    encodedBytes: number,
    bufferedBytes: number,
  ): void => {
    if (ws.data.pressureClosing) return;
    const stats = applicationStats(ws);
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    signal("sync.ws_frame_dropped", {
      caller_fp: ws.data.caller.fingerprint,
      frame,
      bytes: encodedBytes,
      buffered_bytes: bufferedBytes,
      unacked_frames: stats.unackedFrames,
      unacked_bytes: stats.unackedBytes,
      oldest_age_ms: stats.oldestAgeMs,
      cooldownKey: ws.data.caller.fingerprint,
    });
    ws.close(1013, "sync backpressure");
  };

  const sendGuarded = (ws: ServerWebSocket<SyncWsData>, frame: FirehoseFrame): boolean => {
    if (ws.data.pressureClosing) return false;
    try {
      const frameKind = frame.frame.case ?? "unknown";
      const nextSeq = ws.data.lastSentDeliverySeq + 1n;
      const outboundFrame = ws.data.flowControl
        ? create(FirehoseFrameSchema, { deliverySeq: nextSeq, frame: frame.frame })
        : frame;
      const bin = toBinary(FirehoseFrameSchema, outboundFrame);

      if (ws.data.flowControl) {
        if (ws.data.deliveryQueue.length + 1 > APPLICATION_MAX_UNACKED_FRAMES) {
          closeForBackpressure(ws, "frame_limit", frameKind);
          return false;
        }
        if (ws.data.unackedEncodedBytes + bin.byteLength > APPLICATION_MAX_UNACKED_BYTES) {
          closeForBackpressure(ws, "byte_limit", frameKind);
          return false;
        }
      }

      const sentAtMs = deadlineClock.now();
      const result = ws.send(bin);
      if (result !== 0 && ws.data.flowControl) {
        ws.data.lastSentDeliverySeq = nextSeq;
        ws.data.unackedEncodedBytes += bin.byteLength;
        ws.data.deliveryQueue.push({ seq: nextSeq, encodedBytes: bin.byteLength, sentAtMs });
        if (!ws.data.deliveryTimer) rearmApplicationDeadline(ws);
      }

      const bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, frameKind, bin.byteLength, bufferedBytes);
        return false;
      }
      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", frameKind);
        return false;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = frameKind;
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? frameKind);
        }, backpressureTimeoutMs);
      }
      return true;
    } catch (e) {
      log.warn("sync-ws", "send_failed", { error: String(e) });
      return false;
    }
  };

  const waitForDeliveryChange = (
    ws: ServerWebSocket<SyncWsData>,
  ): Promise<void> => {
    const { promise, resolve } = Promise.withResolvers<void>();
    if (ws.data.pressureClosing) {
      resolve();
    } else {
      ws.data.deliveryWaiters.add(resolve);
    }
    return promise;
  };
  const pushPacedSeed = async (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
  ): Promise<boolean> => {
    // A retained seed is allowed onto the wire only when prior application
    // work has been cumulatively ACKed. This is ACK-coupled rather than a
    // fixed chunk, so an arbitrarily large seed cannot outrun the 512/4 MiB
    // window while live frames remain queued in startSyncFeed.
    while (!ws.data.pressureClosing && ws.data.deliveryQueue.length > 0) {
      await waitForDeliveryChange(ws);
    }
    if (ws.data.pressureClosing) return false;

    if (!sendGuarded(ws, frame)) {
      if (!ws.data.pressureClosing) {
        ws.data.pressureClosing = true;
        cleanupSocket(ws);
        ws.close(1013, "sync backpressure");
      }
      return false;
    }

    const sentSeq = ws.data.lastSentDeliverySeq;
    while (!ws.data.pressureClosing && ws.data.ackDeliverySeq < sentSeq) {
      await waitForDeliveryChange(ws);
    }
    return !ws.data.pressureClosing;
  };

  const closeForInvalidAck = (ws: ServerWebSocket<SyncWsData>): void => {
    if (ws.data.pressureClosing) return;
    ws.data.pressureClosing = true;
    cleanupSocket(ws);
    ws.close(1008, "invalid sync ack");
  };

  const applyCumulativeAck = (
    ws: ServerWebSocket<SyncWsData>,
    ackDeliverySeq: bigint,
  ): boolean => {
    if (ackDeliverySeq > ws.data.lastSentDeliverySeq) {
      closeForInvalidAck(ws);
      return false;
    }
    if (ackDeliverySeq <= ws.data.ackDeliverySeq) return true;
    let releaseCount = 0;
    let releasedBytes = 0;
    for (const record of ws.data.deliveryQueue) {
      if (record.seq > ackDeliverySeq) break;
      releaseCount += 1;
      releasedBytes += record.encodedBytes;
    }
    if (releaseCount > 0) ws.data.deliveryQueue.splice(0, releaseCount);
    ws.data.unackedEncodedBytes -= releasedBytes;
    ws.data.ackDeliverySeq = ackDeliverySeq;
    const v2 = ws.data.v2;
    if (v2) {
      for (const [sessionId, deliverySeq] of v2.pendingSessionAnnouncements) {
        if (deliverySeq <= ackDeliverySeq) v2.pendingSessionAnnouncements.delete(sessionId);
      }
    }
    rearmApplicationDeadline(ws);
    wakeDeliveryWaiters(ws);
    if (v2) scheduleV2(ws);
    return true;
  };

  return {
    clearNativePressure,
    clearApplicationWindow,
    closeForBackpressure,
    closeForDroppedFrame,
    rearmApplicationDeadline,
    sendGuarded,
    pushPacedSeed,
    closeForInvalidAck,
    applyCumulativeAck,
  };
}
