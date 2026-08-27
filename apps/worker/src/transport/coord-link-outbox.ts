// Encoded-outbox + native-backpressure engine for coord-link.ts. Owns every
// byte that leaves the worker: the two bounded pending lanes (control and raw
// metadata) and the byte admission that decides whether a frame goes straight
// onto the native socket or waits. The at-least-once SessionEvent ledger sits
// behind it in coord-link-unacked.ts. Extracted from coord-link.ts as pure
// code motion; the factory is per-link, so all state stays per-socket.
//
// drainQueues()'s ordering is load-bearing and documented inline: durable and
// control chronology always fences cells and raw metadata, which is what
// preserves `opened` -> first full when native buffering is saturated. Do not
// reorder it.

import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WCellGridSchema, WCellGridChunkSchema, WAgentStatusSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { frameToProto, binaryFrameToProto } from "./coord-link-codec.ts";
import { createCoordLinkUnacked } from "./coord-link-unacked.ts";
import {
  PENDING_CAP, PENDING_BYTES_CAP, RAW_METADATA_MAX_AGE_MS,
  WS_BUFFERED_HIGH_WATER_BYTES, WS_DRAIN_RETRY_MS,
} from "./coord-link-constants.ts";
import type {
  CoordLinkDeps, CoordLinkOutbox, TerminalCellSendResult, TransportSendResult, UpstreamFrame,
} from "./coord-link-types.ts";

interface EncodedPending {
  bytes: Uint8Array;
  queuedAtMs: number;
  kind: "control" | "raw";
}

export function createCoordLinkOutbox(
  deps: CoordLinkDeps,
  isDisposed: () => boolean,
): CoordLinkOutbox {
  // `writer` accepts already-encoded bytes. Admission/backpressure checks live
  // in tryWriteEncoded(), so every queued byte is counted exactly once.
  let writer: ((bytes: Uint8Array) => void) | null = null;
  let activeWs: WebSocket | null = null;
  let linkReady = false;
  let drainTimer: NodeJS.Timeout | null = null;
  let pendingFrameCount = 0;
  let pendingEncodedBytes = 0;
  const controlPending: EncodedPending[] = [];
  const rawPending: EncodedPending[] = [];
  let writableNotificationPending = false;
  let notifyingWritable = false;
  const events = createCoordLinkUnacked({
    isDisposed,
    encodeUpstream: (frame) => encodeUpstream(frame),
    tryWriteEncoded: (bytes) => tryWriteEncoded(bytes),
    isAttached: () => writer !== null,
    kick: () => { drainQueues(); },
  });

  function clearDrainTimer(): void {
    if (drainTimer !== null) { clearTimeout(drainTimer); drainTimer = null; }
  }

  function encodeUpstream(frame: CoordWorkerUp): Uint8Array | null {
    try {
      return toBinary(CoordWorkerUpSchema, frame);
    } catch (error) {
      log.warn("coord-link", "upstream_encode_failed", {
        error: error instanceof Error ? error.message : String(error),
      });
      return null;
    }
  }

  function nativeHasCapacity(byteLength: number): boolean {
    if (
      !writer ||
      !activeWs ||
      activeWs.readyState !== WebSocket.OPEN ||
      byteLength > PENDING_BYTES_CAP
    ) return false;
    const buffered = activeWs.bufferedAmount;
    // Permit one large (but bounded) frame when the native queue is empty.
    // Otherwise stop before crossing the high-water mark.
    return buffered === 0
      ? byteLength <= PENDING_BYTES_CAP
      : buffered + byteLength <= WS_BUFFERED_HIGH_WATER_BYTES;
  }

  function tryWriteEncoded(bytes: Uint8Array): boolean {
    if (!nativeHasCapacity(bytes.byteLength) || !writer) return false;
    try {
      writer(bytes);
      return true;
    } catch (error) {
      diag("transport.frame_dropped", {
        reason: "writer_throw",
        kind: "encoded",
        bytes: bytes.byteLength,
      });
      log.warn("coord-link", "writer_throw", {
        error: error instanceof Error ? error.message : String(error),
      });
      return false;
    }
  }

  function scheduleDrain(): void {
    if (isDisposed() || !writer || drainTimer !== null) return;
    drainTimer = setTimeout(drainQueues, WS_DRAIN_RETRY_MS);
  }

  function enqueueEncoded(kind: EncodedPending["kind"], bytes: Uint8Array): boolean {
    if (
      pendingFrameCount >= PENDING_CAP ||
      pendingEncodedBytes + bytes.byteLength > PENDING_BYTES_CAP
    ) {
      diag("transport.frame_dropped", {
        reason: pendingFrameCount >= PENDING_CAP ? "pending_frame_overflow" : "pending_byte_overflow",
        kind,
        frames: pendingFrameCount,
        bytes: pendingEncodedBytes,
        frame_bytes: bytes.byteLength,
      });
      return false;
    }
    const item: EncodedPending = { bytes, queuedAtMs: Date.now(), kind };
    (kind === "raw" ? rawPending : controlPending).push(item);
    pendingFrameCount += 1;
    pendingEncodedBytes += bytes.byteLength;
    scheduleDrain();
    return true;
  }

  function removePendingHead(queue: EncodedPending[]): EncodedPending | undefined {
    const item = queue.shift();
    if (!item) return undefined;
    pendingFrameCount -= 1;
    pendingEncodedBytes -= item.bytes.byteLength;
    return item;
  }

  function rawMetadataAged(now = Date.now()): boolean {
    const oldest = rawPending[0];
    return oldest !== undefined && now - oldest.queuedAtMs >= RAW_METADATA_MAX_AGE_MS;
  }

  function drainControls(): void {
    while (controlPending.length > 0) {
      const item = controlPending[0]!;
      if (!tryWriteEncoded(item.bytes)) return;
      removePendingHead(controlPending);
    }
  }

  function drainOneRaw(): boolean {
    const item = rawPending[0];
    if (!item || !tryWriteEncoded(item.bytes)) return false;
    removePendingHead(rawPending);
    return true;
  }

  function maybeNotifyWritable(): void {
    if (
      !writableNotificationPending ||
      notifyingWritable ||
      !linkReady ||
      events.unsentCount() > 0 ||
      !nativeHasCapacity(0)
    ) return;
    writableNotificationPending = false;
    notifyingWritable = true;
    try {
      deps.onWritable?.();
    } catch (error) {
      log.warn("coord-link", "on_writable_failed", {
        error: error instanceof Error ? error.message : String(error),
      });
    } finally {
      notifyingWritable = false;
    }
  }

  function drainQueues(): void {
    clearDrainTimer();
    if (isDisposed() || !writer) return;
    // Durable/control chronology always fences cells and raw metadata. This is
    // what preserves opened -> first full when native buffering is saturated.
    events.drainUnsent();
    if (events.unsentCount() > 0) {
      scheduleDrain();
      return;
    }
    // A cell that was dropped behind an earlier durable event is repaired
    // before later RPC/control replies. This preserves opened -> first full ->
    // spawn result even when the native socket was saturated at opened.
    if (linkReady) {
      maybeNotifyWritable();
      if (writableNotificationPending) {
        scheduleDrain();
        return;
      }
    }
    drainControls();
    if (controlPending.length > 0) {
      scheduleDrain();
      return;
    }
    // Raw frames are held until helloAck. Authoritative repairs above lead the
    // reconnect backlog.
    if (!linkReady) return;
    if (rawMetadataAged()) drainOneRaw();
    while (rawPending.length > 0 && drainOneRaw()) { /* FIFO */ }
    if (rawPending.length > 0 || writableNotificationPending) scheduleDrain();
  }

  function sendControlProto(frame: CoordWorkerUp): TransportSendResult {
    const bytes = encodeUpstream(frame);
    if (!bytes) return "dropped";
    if (
      events.unsentCount() === 0 &&
      controlPending.length === 0 &&
      tryWriteEncoded(bytes)
    ) return "sent";
    return enqueueEncoded("control", bytes) ? "queued" : "dropped";
  }

  function send(frame: UpstreamFrame): boolean {
    if (isDisposed()) return false;
    if (frame.kind === "event") return events.send(frame.event);
    const proto = frameToProto(frame);
    return proto ? sendControlProto(proto) === "sent" : false;
  }

  function sendBinary(
    channelId: number,
    direction: number,
    endSeq: number,
    data: Uint8Array,
  ): TransportSendResult {
    if (isDisposed()) return "dropped";
    const bytes = encodeUpstream(binaryFrameToProto(channelId, direction, endSeq, data));
    if (!bytes) return "dropped";
    if (
      linkReady &&
      events.unsentCount() === 0 &&
      controlPending.length === 0 &&
      rawPending.length === 0 &&
      tryWriteEncoded(bytes)
    ) return "sent";
    return enqueueEncoded("raw", bytes) ? "queued" : "dropped";
  }

  function sendCellGrid(channelId: number, frame: PbCellGridFrame): TerminalCellSendResult {
    if (isDisposed()) return "dropped";
    if (
      !linkReady ||
      !writer ||
      events.unsentCount() > 0 ||
      (controlPending.length > 0 && !notifyingWritable)
    ) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    // Cells normally lead raw metadata. Once raw has waited 100 ms, admit one
    // metadata frame before an ordinary delta so parser input cannot starve.
    // Full repairs always lead reconnect backlog.
    if (!frame.full && rawMetadataAged() && !drainOneRaw()) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "cellGrid", value: create(WCellGridSchema, { channelId, frame }) },
    }));
    if (bytes && tryWriteEncoded(bytes)) return "sent";
    writableNotificationPending = true;
    scheduleDrain();
    diag("transport.frame_dropped", {
      reason: bytes ? "native_backpressure" : "encode",
      kind: "cellGrid",
      channel_id: channelId,
    });
    return "dropped";
  }
  function sendCellGridChunk(channelId: number, chunk: PbCellGridChunk): TerminalCellSendResult {
    if (
      isDisposed() ||
      !linkReady ||
      !writer ||
      events.unsentCount() > 0 ||
      (controlPending.length > 0 && !notifyingWritable)
    ) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "cellGridChunk", value: create(WCellGridChunkSchema, { channelId, chunk }) },
    }));
    if (bytes && tryWriteEncoded(bytes)) return "sent";
    writableNotificationPending = true;
    scheduleDrain();
    diag("transport.frame_dropped", {
      reason: bytes ? "native_backpressure" : "encode",
      kind: "cellGridChunk",
      channel_id: channelId,
    });
    return "dropped";
  }

  function sendAgentStatus(status: AgentStatusUpdate): boolean {
    if (
      isDisposed() ||
      !writer ||
      events.unsentCount() > 0 ||
      controlPending.length > 0
    ) return false;
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "agentStatus", value: create(WAgentStatusSchema, {
        sessionId: status.session_id,
        agentId: status.agent_id,
        state: status.state,
        message: status.message,
        revision: BigInt(status.revision),
        completedRevision: BigInt(status.completed_revision),
        updatedAt: status.updated_at,
        active: status.active,
      }) },
    }));
    return bytes ? tryWriteEncoded(bytes) : false;
  }

  function detachSocket(): void {
    writer = null;
    activeWs = null;
    linkReady = false;
    // Anything still awaiting an application ACK may have been lost with
    // the native socket. Re-admit it on the next dial; coordinator dedup
    // makes replay safe.
    events.requeueAll();
  }

  function reset(): void {
    controlPending.length = 0;
    rawPending.length = 0;
    pendingFrameCount = 0;
    pendingEncodedBytes = 0;
    events.clear();
  }

  return {
    send, sendBinary, sendCellGrid, sendCellGridChunk, sendAgentStatus, sendControlProto,
    encodeUpstream, detachSocket, reset, drainQueues, clearDrainTimer,
    forceWrite: (bytes) => { if (!writer) return false; writer(bytes); return true; },
    attachSocket: (socket, write) => { linkReady = false; activeWs = socket; writer = write; },
    markLinkReady: () => { linkReady = true; },
    isAttached: () => writer !== null,
    activeSocket: () => activeWs,
    replayUnacked: () => { events.replay(); },
    ackEvent: (seq) => { events.ack(seq); },
    unackedCount: () => events.count(),
  };
}
