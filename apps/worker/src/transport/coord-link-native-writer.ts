// Native WebSocket byte admission for the coordinator link. The outbox owns
// ordering and pending queues; this owner exposes capacity-aware writes and the
// one forced hello write while keeping socket-generation state in one place.
import { diag } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import {
  PENDING_BYTES_CAP,
  WS_BUFFERED_HIGH_WATER_BYTES,
} from "./coord-link-constants.ts";

export interface CoordLinkNativeWriter {
  attach(socket: WebSocket, write: (bytes: Uint8Array) => void): void;
  detach(): void;
  isAttached(): boolean;
  activeSocket(): WebSocket | null;
  hasCapacity(byteLength: number): boolean;
  tryWrite(bytes: Uint8Array): boolean;
  forceWrite(bytes: Uint8Array): boolean;
}

export function createCoordLinkNativeWriter(): CoordLinkNativeWriter {
  let writer: ((bytes: Uint8Array) => void) | null = null;
  let currentSocket: WebSocket | null = null;

  function hasCapacity(byteLength: number): boolean {
    if (
      !writer
      || !currentSocket
      || currentSocket.readyState !== WebSocket.OPEN
      || byteLength > PENDING_BYTES_CAP
    ) return false;
    const buffered = currentSocket.bufferedAmount;
    return buffered === 0
      ? byteLength <= PENDING_BYTES_CAP
      : buffered + byteLength <= WS_BUFFERED_HIGH_WATER_BYTES;
  }

  return {
    attach(socket, write) {
      currentSocket = socket;
      writer = write;
    },
    detach() {
      writer = null;
      currentSocket = null;
    },
    isAttached() {
      return writer !== null;
    },
    activeSocket() {
      return currentSocket;
    },
    hasCapacity,
    tryWrite(bytes) {
      if (!hasCapacity(bytes.byteLength) || !writer) return false;
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
    },
    forceWrite(bytes) {
      if (!writer) return false;
      writer(bytes);
      return true;
    },
  };
}
