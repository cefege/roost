// Encoded-outbox and pending-lane engine for coord-link.ts. It orders durable
// replay, agent status, controls, cells, legacy raw metadata, and coalesced
// semantic metadata around the native byte writer.
// drainQueues() preserves durable/control chronology before terminal frames so
// an opened event always precedes its first terminal publication.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WCellGridSchema, WCellGridChunkSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  frameToProto,
  binaryFrameToProto,
  terminalMetadataFrameToProto,
} from "./coord-link-codec.ts";
import { createCoordLinkAgentStatusOutbox } from "./coord-link-agent-status.ts";
import { createCoordLinkTerminalMetadataOutbox } from "./coord-link-terminal-metadata.ts";
import {
  PENDING_CAP, PENDING_BYTES_CAP, RAW_METADATA_MAX_AGE_MS, WS_DRAIN_RETRY_MS,
} from "./coord-link-constants.ts";
import { createCoordLinkNativeWriter } from "./coord-link-native-writer.ts";
import { createCoordLinkUnacked } from "./coord-link-unacked.ts";
import type {
  CoordLinkDeps, CoordLinkOutbox, CoordLinkPipelineState, TerminalCellSendResult,
  TerminalMetadataFrame, TransportSendResult, UpstreamFrame,
} from "./coord-link-types.ts";
interface EncodedPending {
  bytes: Uint8Array;
  queuedAtMs: number;
  kind: "liveness" | "control" | "raw";
}
export function createCoordLinkOutbox(
  deps: CoordLinkDeps,
  isDisposed: () => boolean,
): CoordLinkOutbox {
  const nativeWriter = createCoordLinkNativeWriter();
  let linkReady = false;
  let terminalMetadataNegotiated = false;
  let drainTimer: NodeJS.Timeout | null = null;
  let pendingFrameCount = 0;
  let pendingEncodedBytes = 0;
  const livenessPending: EncodedPending[] = [];
  const controlPending: EncodedPending[] = [];
  const rawPending: EncodedPending[] = [];
  let writableNotificationPending = false;
  let notifyingWritable = false;
  const events = createCoordLinkUnacked(deps.sessionEventStore, {
    isDisposed,
    encodeUpstream: (frame) => encodeUpstream(frame),
    tryWriteEncoded: nativeWriter.tryWrite,
    isAttached: nativeWriter.isAttached,
    kick: () => { drainQueues(); },
    onLive: (reconnected) => {
      linkReady = true;
      deps.onSnapshotReady?.({ reconnected });
      drainQueues();
    },
  });
  const agentStatuses = createCoordLinkAgentStatusOutbox({
    encodeUpstream,
    tryWriteEncoded: nativeWriter.tryWrite,
    scheduleDrain,
  });
  const terminalMetadata = createCoordLinkTerminalMetadataOutbox({
    encode: (metadata) => encodeUpstream(terminalMetadataFrameToProto(metadata)),
    tryWriteEncoded: nativeWriter.tryWrite,
    scheduleDrain,
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
  function scheduleDrain(): void {
    if (isDisposed() || !nativeWriter.isAttached() || drainTimer !== null) return;
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
    (kind === "raw" ? rawPending : kind === "liveness" ? livenessPending : controlPending).push(item);
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
  function drainLiveness(): void {
    while (livenessPending.length > 0) {
      const item = livenessPending[0]!;
      if (!nativeWriter.tryWrite(item.bytes)) return;
      removePendingHead(livenessPending);
    }
  }
  function drainControls(): void {
    while (controlPending.length > 0) {
      const item = controlPending[0]!;
      if (!nativeWriter.tryWrite(item.bytes)) return;
      removePendingHead(controlPending);
    }
  }

  function drainOneRaw(): boolean {
    const item = rawPending[0];
    if (!item || !nativeWriter.tryWrite(item.bytes)) return false;
    removePendingHead(rawPending);
    return true;
  }

  function maybeNotifyWritable(): void {
    if (
      !writableNotificationPending ||
      notifyingWritable ||
      !linkReady ||
      events.unsentCount() > 0 ||
      !nativeWriter.hasCapacity(0)
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
    if (isDisposed() || !nativeWriter.isAttached()) return;
    drainLiveness();
    if (livenessPending.length > 0) {
      scheduleDrain();
      return;
    }
    events.drainUnsent();
    if (events.unsentCount() > 0) {
      scheduleDrain();
      return;
    }
    if (!linkReady) return;
    maybeNotifyWritable();
    if (writableNotificationPending) {
      scheduleDrain();
      return;
    }
    agentStatuses.drain();
    if (agentStatuses.hasPending()) {
      scheduleDrain();
      return;
    }
    drainControls();
    if (controlPending.length > 0) {
      scheduleDrain();
      return;
    }
    if (rawMetadataAged()) drainOneRaw();
    while (rawPending.length > 0 && drainOneRaw()) { /* FIFO */ }
    const metadataDrained = terminalMetadata.drain();
    if (metadataDrained) {
      writableNotificationPending = true;
      maybeNotifyWritable();
    }
    if (rawPending.length > 0 || terminalMetadata.hasPending() || writableNotificationPending) {
      scheduleDrain();
    }
  }

  function sendControlProto(frame: CoordWorkerUp): TransportSendResult {
    const bytes = encodeUpstream(frame);
    if (!bytes) return "dropped";
    if (
      linkReady &&
      events.unsentCount() === 0 &&
      controlPending.length === 0 &&
      !agentStatuses.hasPending() &&
      nativeWriter.tryWrite(bytes)
    ) return "sent";
    return enqueueEncoded("control", bytes) ? "queued" : "dropped";
  }

  function sendLivenessProto(frame: CoordWorkerUp): TransportSendResult {
    const bytes = encodeUpstream(frame);
    if (!bytes) return "dropped";
    if (livenessPending.length === 0 && nativeWriter.tryWrite(bytes)) return "sent";
    return enqueueEncoded("liveness", bytes) ? "queued" : "dropped";
  }

  function send(frame: UpstreamFrame): boolean {
    if (isDisposed()) return false;
    if (frame.kind === "event") {
      return events.send(frame.event, frame.clientSeq, frame.eventClass, frame.metadataKey);
    }
    const proto = frameToProto(frame);
    if (!proto) return false;
    return (frame.kind === "pong" ? sendLivenessProto(proto) : sendControlProto(proto)) === "sent";
  }

  function sendBinary(
    channelId: number,
    direction: number,
    endSeq: number,
    data: Uint8Array,
  ): TransportSendResult {
    if (isDisposed()) return "dropped";
    if (terminalMetadataNegotiated) return "dropped";
    const bytes = encodeUpstream(binaryFrameToProto(channelId, direction, endSeq, data));
    if (!bytes) return "dropped";
    if (
      linkReady &&
      events.unsentCount() === 0 &&
      controlPending.length === 0 &&
      !agentStatuses.hasPending() &&
      rawPending.length === 0 &&
      nativeWriter.tryWrite(bytes)
    ) return "sent";
    return enqueueEncoded("raw", bytes) ? "queued" : "dropped";
  }

  function sendTerminalMetadata(metadata: TerminalMetadataFrame): TransportSendResult {
    if (isDisposed() || !terminalMetadataNegotiated) return "dropped";
    return terminalMetadata.send(metadata,
      linkReady
      && events.unsentCount() === 0
      && controlPending.length === 0
      && !agentStatuses.hasPending()
      && rawPending.length === 0);
  }

  function sendCellGrid(channelId: number, frame: PbCellGridFrame): TerminalCellSendResult {
    if (isDisposed()) return "dropped";
    if (
      !linkReady ||
      !nativeWriter.isAttached() ||
      events.unsentCount() > 0 ||
      ((controlPending.length > 0 || agentStatuses.hasPending()) && !notifyingWritable)
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
    if (bytes && nativeWriter.tryWrite(bytes)) return "sent";
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
      !nativeWriter.isAttached() ||
      events.unsentCount() > 0 ||
      ((controlPending.length > 0 || agentStatuses.hasPending()) && !notifyingWritable)
    ) {
      writableNotificationPending = true;
      scheduleDrain();
      return "dropped";
    }
    const bytes = encodeUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "cellGridChunk", value: create(WCellGridChunkSchema, { channelId, chunk }) },
    }));
    if (bytes && nativeWriter.tryWrite(bytes)) return "sent";
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
    if (isDisposed()) return false;
    return agentStatuses.send(status,
      linkReady
      && nativeWriter.isAttached()
      && events.unsentCount() === 0
      && controlPending.length === 0
      && !writableNotificationPending);
  }

  function detachSocket(): void {
    nativeWriter.detach();
    linkReady = false;
    terminalMetadataNegotiated = false;
    while (livenessPending.length > 0) removePendingHead(livenessPending);
    while (controlPending.length > 0) removePendingHead(controlPending);
    while (rawPending.length > 0) removePendingHead(rawPending);
    terminalMetadata.disconnect();
    writableNotificationPending = false;
    events.disconnect();
    agentStatuses.disconnect();
  }

  function reset(): void {
    livenessPending.length = 0;
    controlPending.length = 0;
    rawPending.length = 0;
    terminalMetadata.clear();
    terminalMetadataNegotiated = false;
    pendingFrameCount = 0;
    pendingEncodedBytes = 0;
    events.clear();
    agentStatuses.clear();
  }

  function pipelineState(): CoordLinkPipelineState {
    const nativeBufferedBytes = nativeWriter.activeSocket()?.bufferedAmount ?? 0;
    return {
      queueFrames: pendingFrameCount,
      queueBytes: pendingEncodedBytes,
      nativeBufferedBytes: Number.isFinite(nativeBufferedBytes) && nativeBufferedBytes > 0
        ? Math.floor(nativeBufferedBytes)
        : 0,
      attached: nativeWriter.isAttached(),
    };
  }

  return {
    send, sendBinary, sendTerminalMetadata, sendCellGrid, sendCellGridChunk, sendAgentStatus,
    sendControlProto, sendLivenessProto,
    encodeUpstream, detachSocket, reset, pipelineState, drainQueues, clearDrainTimer,
    forceWrite: nativeWriter.forceWrite,
    attachSocket: (socket, write) => {
      linkReady = false;
      terminalMetadataNegotiated = false;
      nativeWriter.attach(socket, write);
    },
    acceptHelloAck: (reconnected, metadataNegotiated = false) => {
      terminalMetadataNegotiated = metadataNegotiated;
      if (metadataNegotiated) while (rawPending.length > 0) removePendingHead(rawPending);
      events.acceptHelloAck(reconnected);
      drainQueues();
    },
    activateSnapshotProvider: (provider) => { events.activateSnapshotProvider(provider); drainQueues(); },
    snapshotStateChanged: () => { events.snapshotStateChanged(); drainQueues(); },
    protocolPhase: () => events.phase(),
    ready: () => events.ready(),
    waitForDurableSessionEventReplay: (signal) =>
      events.waitForDurableSessionEventReplay(signal),
    isAttached: nativeWriter.isAttached,
    activeSocket: nativeWriter.activeSocket,
    ackEvent: (seq) => { events.ack(seq); drainQueues(); },
  };
}
