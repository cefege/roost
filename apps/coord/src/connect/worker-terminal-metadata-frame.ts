// Routes worker terminal metadata after connection generation checks.
// New negotiated records feed semantic hubs directly; legacy WBinary is parsed
// only by the compatibility adapter and is rejected after capability cutover.
// This module never retains or projects PTY byte payloads to a browser path.

import type { WBinary, WTerminalMetadata } from "@roost/shared/proto/worker_transport_pb";
import { asChannelId, asWorkerFp, DIR_FROM_PTY } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import {
  acceptLegacyTerminalMetadata,
  acceptTerminalMetadata,
} from "../terminal-metadata-adapter.ts";

export function dispatchLegacyTerminalMetadataFrame(
  workerFp: string,
  metadataNegotiated: boolean,
  binary: WBinary,
): void {
  if (binary.direction !== DIR_FROM_PTY) return;
  if (metadataNegotiated) {
    diag("worker.frame_dropped", { reason: "legacy_metadata_after_negotiation", worker_fp: workerFp });
    return;
  }
  acceptLegacyTerminalMetadata(asWorkerFp(workerFp), asChannelId(binary.channelId), binary.data);
}

export function dispatchTerminalMetadataFrame(
  workerFp: string,
  metadataNegotiated: boolean,
  metadata: WTerminalMetadata,
): void {
  if (!metadataNegotiated) {
    diag("worker.frame_dropped", {
      reason: "terminal_metadata_without_negotiation",
      worker_fp: workerFp,
    });
    return;
  }
  acceptTerminalMetadata(asWorkerFp(workerFp), metadata);
}
