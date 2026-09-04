// Sync v2 control-frame send path and native socket backpressure handling.

import type { ServerWebSocket } from "bun";
import { toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { WsDeadlineClock } from "./ws-auth-deadline.ts";
import type { SyncBackpressureReason } from "./sync-ws-v1-delivery.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

interface SyncV2ControlSenderDeps {
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
}

export function makeSyncV2ControlSender(deps: SyncV2ControlSenderDeps) {
  const {
    deadlineClock,
    backpressureLimitBytes,
    backpressureTimeoutMs,
    closeForBackpressure,
    closeForDroppedFrame,
  } = deps;

  return (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
  ): boolean => {
    if (!ws.data.v2 || ws.data.pressureClosing) return false;
    const frameKind = frame.frame.case ?? "control";
    let encodedBytes = 0;
    let bufferedBytes = 0;
    try {
      frame.deliverySeq = 0n;
      frame.domain = SyncDomain.UNSPECIFIED;
      frame.domainGeneration = 0n;
      const binary = toBinary(FirehoseFrameSchema, frame);
      encodedBytes = binary.byteLength;
      const result = ws.send(binary);
      bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, frameKind, encodedBytes, bufferedBytes);
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
    } catch (error) {
      log.warn("sync-ws", "control_send_failed", {
        error: String(error),
        frame: frameKind,
        encoded_bytes: encodedBytes,
      });
      // A transport throw is an ambiguous delivery, so retire this socket
      // instead of allowing the caller to continue after a silent false.
      closeForDroppedFrame(ws, frameKind, encodedBytes, bufferedBytes);
      return false;
    }
  };
}
