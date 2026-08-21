import {
  KEEPER_MAX_HISTORY_RESIZE_RECORDS,
  MuxFrameType,
  encodeMuxFrame,
  encodeResizeResult,
} from "./protocol.ts";
import type { ResizeWireResult } from "./protocol.ts";
import type { Channel, ClientState } from "./keeper-types.ts";

const RESIZE_STATUS_CACHE_MAX = KEEPER_MAX_HISTORY_RESIZE_RECORDS;

export function sendResizeResult(
  socket: ClientState["socket"],
  channelId: number,
  result: ResizeWireResult,
): void {
  const frameType = result.kind === "ack" ? MuxFrameType.ResizeAck : MuxFrameType.ResizeReject;
  try {
    socket.write(encodeMuxFrame(frameType, channelId, encodeResizeResult(result)));
  } catch {
    // Cached status remains queryable after the worker reconnects.
  }
}

export function cacheResizeResult(ch: Channel, result: ResizeWireResult): void {
  if (!ch.resizeStatuses.has(result.seq)
      && ch.resizeStatuses.size >= RESIZE_STATUS_CACHE_MAX) {
    const oldest = ch.resizeStatuses.keys().next().value as number | undefined;
    if (oldest !== undefined) ch.resizeStatuses.delete(oldest);
  }
  ch.resizeStatuses.set(result.seq, result);
  ch.highestResizeSeq = Math.max(ch.highestResizeSeq, result.seq);
  if (result.kind === "ack") ch.appliedResizeSeq = Math.max(ch.appliedResizeSeq, result.seq);
}
