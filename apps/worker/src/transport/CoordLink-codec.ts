// Pure frame codecs for CoordLink.ts — stateless conversions between the
// caller-facing UpstreamFrame / binary layout and wire CoordWorkerUp protos.
// Extracted to keep CoordLink.ts under the 400-line cap; none of these close
// over the factory's mutable state, so they take all inputs as parameters.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WHelloSchema, WPongSchema, WBinarySchema,
  WRpcOkSchema, WRpcErrorSchema, WTransferLineSchema, WTransferDoneSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { UpstreamFrame } from "./CoordLink-types.ts";

export function frameToProto(f: UpstreamFrame): CoordWorkerUp | null {
  switch (f.kind) {
    case "hello":
      return create(CoordWorkerUpSchema, { frame: { case: "hello", value: create(WHelloSchema, { workerFp: f.worker_fp, version: f.version }) }});
    case "pong":
      return create(CoordWorkerUpSchema, { frame: { case: "pong", value: create(WPongSchema, { ts: BigInt(f.ts) }) }});
    // "event" kind no longer routes through frameToProto — sendEvent
    // owns seq assignment + unacked bookkeeping so writer-throw
    // doesn't double-allocate seqs or strand unacked entries.
    case "event":
      return null;
    case "rpc-ok":
      return create(CoordWorkerUpSchema, { frame: { case: "rpcOk", value: create(WRpcOkSchema, {
        requestId: f.request_id, dataJson: JSON.stringify(f.data),
      })}});
    case "rpc-error":
      return create(CoordWorkerUpSchema, { frame: { case: "rpcError", value: create(WRpcErrorSchema, {
        requestId: f.request_id, message: f.message,
      })}});
    case "transfer-line":
      return create(CoordWorkerUpSchema, { frame: { case: "transferLine", value: create(WTransferLineSchema, {
        jobId: f.job_id, text: f.text,
      })}});
    case "transfer-done":
      return create(CoordWorkerUpSchema, { frame: { case: "transferDone", value: create(WTransferDoneSchema, {
        jobId: f.job_id, exit: f.exit ?? -1, error: f.error ?? "",
      })}});
  }
}

// session-manager.emitUpstreamChunk frame layout (fixed-size 11-byte
// header): [ch(2)][dir(1)][end_seq(8)][bytes]. Single helper used by
// both the live send path and the reconnect drain so the layout
// can't drift again — prior bug: the drain copy still sliced
// arr.subarray(3) with seq=0 after sendBinary was fixed to 11+seq,
// re-introducing the `00 00 00 00 00 00 XX YY` garbage prefix on
// every reconnect with buffered binary.
export function decodeBinaryFrame(arr: Uint8Array): { ch: number; dir: number; seq: bigint; data: Uint8Array } | null {
  if (arr.length < 11) return null;
  const ch = (arr[0]! << 8) | arr[1]!;
  const dir = arr[2]!;
  // Single DataView.getBigUint64 call instead of an 8-iteration BigInt
  // shift-accumulate loop. Fires on every PTY chunk + every drained
  // item on reconnect.
  const seq = new DataView(arr.buffer, arr.byteOffset + 3, 8).getBigUint64(0, false);
  return { ch, dir, seq, data: arr.subarray(11) };
}

export function binaryFrameToProto(f: { ch: number; dir: number; seq: bigint; data: Uint8Array }): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "binary", value: create(WBinarySchema, {
      channelId: f.ch, direction: f.dir, seq: f.seq, data: f.data,
    })},
  });
}
