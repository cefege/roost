// Coalescing outbound lane for compact terminal metadata records.
// It retains one latest encoded semantic record per channel across native
// backpressure, then drops its bounded state on reconnect for manager replay.
// PTY byte buffers never enter this lane.

import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import { diag } from "@roost/shared/diag";
import { WORKER_SNAPSHOT_MAX_SESSIONS } from "./coord-link-constants.ts";
import type { TerminalMetadataFrame, TransportSendResult } from "./coord-link-types.ts";

interface EncodedTerminalMetadata {
  bytes: Uint8Array;
  metadata: TerminalMetadataFrame;
}

export interface CoordLinkTerminalMetadataOutbox {
  send(metadata: TerminalMetadataFrame, canWriteDirect: boolean): TransportSendResult;
  drain(): boolean;
  disconnect(): void;
  clear(): void;
  hasPending(): boolean;
}

interface CoordLinkTerminalMetadataOptions {
  encode(metadata: TerminalMetadataFrame): Uint8Array | null;
  tryWriteEncoded(bytes: Uint8Array): boolean;
  scheduleDrain(): void;
}

const TERMINAL_METADATA_PENDING_CAP = WORKER_SNAPSHOT_MAX_SESSIONS;
const TERMINAL_METADATA_PENDING_BYTES_CAP = WORKER_SNAPSHOT_MAX_SESSIONS * 2_048;

export function createCoordLinkTerminalMetadataOutbox(
  options: CoordLinkTerminalMetadataOptions,
): CoordLinkTerminalMetadataOutbox {
  const pendingByChannel = new Map<number, EncodedTerminalMetadata>();
  let pendingBytes = 0;

  function send(metadata: TerminalMetadataFrame, canWriteDirect: boolean): TransportSendResult {
    const previous = pendingByChannel.get(metadata.channelId);
    const merged: TerminalMetadataFrame = {
      channelId: metadata.channelId,
      titleChanged: metadata.titleChanged || previous?.metadata.titleChanged === true,
      title: metadata.titleChanged ? metadata.title : previous?.metadata.title ?? "",
      activityChanged: metadata.activityChanged || previous?.metadata.activityChanged === true,
      activityTsMs: metadata.activityChanged
        ? metadata.activityTsMs
        : previous?.metadata.activityTsMs ?? 0,
    };
    const bytes = options.encode(merged);
    if (!bytes) return "dropped";
    if (canWriteDirect && !previous && options.tryWriteEncoded(bytes)) return "sent";
    const nextBytes = pendingBytes - (previous?.bytes.byteLength ?? 0) + bytes.byteLength;
    if (
      (!previous && pendingByChannel.size >= TERMINAL_METADATA_PENDING_CAP)
      || nextBytes > TERMINAL_METADATA_PENDING_BYTES_CAP
    ) {
      diag("transport.frame_dropped", {
        reason: !previous ? "terminal_metadata_frame_overflow" : "terminal_metadata_byte_overflow",
        channel_id: metadata.channelId,
        frames: pendingByChannel.size,
        bytes: pendingBytes,
        frame_bytes: bytes.byteLength,
      });
      return "dropped";
    }
    pendingByChannel.set(metadata.channelId, { bytes, metadata: merged });
    pendingBytes = nextBytes;
    options.scheduleDrain();
    return "queued";
  }

  function drain(): boolean {
    let drained = false;
    while (pendingByChannel.size > 0) {
      const next = pendingByChannel.entries().next().value;
      if (!next) return drained;
      const [channelId, encoded] = next;
      if (!options.tryWriteEncoded(encoded.bytes)) return drained;
      pendingByChannel.delete(channelId);
      pendingBytes -= encoded.bytes.byteLength;
      drained = true;
    }
    return drained;
  }

  function disconnect(): void {
    pendingByChannel.clear();
    pendingBytes = 0;
  }

  return {
    send,
    drain,
    disconnect,
    clear: disconnect,
    hasPending: () => pendingByChannel.size > 0,
  };
}
