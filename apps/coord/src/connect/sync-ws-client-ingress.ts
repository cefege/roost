// Binary sync client-frame parsing and protocol routing.

import type { ServerWebSocket } from "bun";
import { fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  type SyncClientFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncWsData } from "./sync-ws-handler.ts";

interface SyncWsClientIngressDeps {
  closeForInvalidAck(ws: ServerWebSocket<SyncWsData>): void;
  applyCumulativeAck(
    ws: ServerWebSocket<SyncWsData>,
    ackDeliverySeq: bigint,
  ): boolean;
  handleV2Command(
    ws: ServerWebSocket<SyncWsData>,
    clientFrame: SyncClientFrame,
  ): void;
}

export function makeSyncWsClientIngress(deps: SyncWsClientIngressDeps) {
  const {
    closeForInvalidAck,
    applyCumulativeAck,
    handleV2Command,
  } = deps;

  return (
    ws: ServerWebSocket<SyncWsData>,
    message: string | Buffer,
  ): void => {
    if (!ws.data.flowControl || ws.data.pressureClosing) return;
    let clientFrame: SyncClientFrame;
    try {
      if (typeof message === "string") throw new TypeError("Sync client frame must be binary");
      clientFrame = fromBinary(SyncClientFrameSchema, message, {
        readUnknownFields: false,
      });
      const canonical = toBinary(SyncClientFrameSchema, clientFrame);
      if (
        canonical.byteLength !== message.byteLength
        || canonical.some((byte, index) => byte !== message[index])
      ) throw new TypeError("Sync client frame must be canonical");
    } catch {
      closeForInvalidAck(ws);
      return;
    }

    if (!ws.data.v2) {
      const ack = clientFrame.ackDeliverySeq;
      if (
        ack === undefined
        || ack <= 0n
        || clientFrame.command.case !== undefined
        || clientFrame.socketId !== ""
      ) {
        closeForInvalidAck(ws);
        return;
      }
      applyCumulativeAck(ws, ack);
      return;
    }

    if (clientFrame.socketId !== ws.data.v2.socketId) return;
    const ack = clientFrame.ackDeliverySeq;
    if (ack !== undefined && ack > 0n && !applyCumulativeAck(ws, ack)) return;
    if (
      (ack === undefined || ack === 0n)
      && clientFrame.command.case === undefined
    ) {
      closeForInvalidAck(ws);
      return;
    }
    handleV2Command(ws, clientFrame);
  };
}
