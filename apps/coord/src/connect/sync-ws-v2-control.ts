// Sync v2 control-frame send path and native socket backpressure handling.

import type { ServerWebSocket } from "bun";
import { toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { SyncDeadlineClock } from "./sync-ws-deadline.ts";
import type { SyncBackpressureReason } from "./sync-ws-v1-delivery.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

interface SyncV2ControlSenderDeps {
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
    try {
      frame.deliverySeq = 0n;
      frame.domain = SyncDomain.UNSPECIFIED;
      frame.domainGeneration = 0n;
      const binary = toBinary(FirehoseFrameSchema, frame);
      const result = ws.send(binary);
      const bufferedBytes = ws.getBufferedAmount();
      if (result === 0) {
        closeForDroppedFrame(ws, frame.frame.case ?? "control", binary.byteLength, bufferedBytes);
        return false;
      }
      if (bufferedBytes > backpressureLimitBytes) {
        closeForBackpressure(ws, "high_water", frame.frame.case ?? "control");
        return false;
      }
      if (result === -1 && !ws.data.pressureTimer) {
        ws.data.pressureFrame = frame.frame.case ?? "control";
        ws.data.pressureTimer = deadlineClock.setTimeout(() => {
          ws.data.pressureTimer = null;
          closeForBackpressure(ws, "timeout", ws.data.pressureFrame ?? "control");
        }, backpressureTimeoutMs);
      }
      return true;
    } catch (error) {
      log.warn("sync-ws", "control_send_failed", { error: String(error) });
      return false;
    }
  };
}
