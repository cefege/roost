// Pure frame codecs for coord-link.ts — stateless conversions between the
// caller-facing UpstreamFrame / binary layout and wire CoordWorkerUp protos.
// Extracted to keep coord-link.ts under the 400-line cap; none of these close
// over the factory's mutable state, so they take all inputs as parameters.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WHelloSchema, WPongSchema, WBinarySchema,
  WRpcOkSchema, WRpcErrorSchema, WTransferLineSchema, WTransferDoneSchema,
  WInputResultSchema, WViewportResultSchema,
  WUpdateProgressSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { UpstreamFrame } from "./coord-link-types.ts";

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
    case "input-result":
      return create(CoordWorkerUpSchema, { frame: { case: "inputResult", value: create(WInputResultSchema, {
        requestId: f.request_id,
        sessionId: f.session_id,
        inputSeq: f.input_seq,
        status: f.status,
        writtenBytes: f.written_bytes,
        reason: f.reason ?? "",
        phase: f.phase,
      })}});
    case "viewport-result":
      return create(CoordWorkerUpSchema, { frame: { case: "viewportResult", value: create(WViewportResultSchema, {
        requestId: f.request_id,
        sessionId: f.session_id,
        clientSeq: f.client_seq,
        status: f.status,
        channelResizeSeq: f.channel_resize_seq,
        cols: f.cols,
        rows: f.rows,
        resized: f.resized,
        reason: f.reason ?? "",
        phase: f.phase,
      })}});
    case "transfer-line":
      return create(CoordWorkerUpSchema, { frame: { case: "transferLine", value: create(WTransferLineSchema, {
        jobId: f.job_id, text: f.text,
      })}});
    case "transfer-done":
      return create(CoordWorkerUpSchema, { frame: { case: "transferDone", value: create(WTransferDoneSchema, {
        jobId: f.job_id, exit: f.exit ?? -1, error: f.error ?? "",
      })}});
    case "update-progress":
      return create(CoordWorkerUpSchema, { frame: { case: "updateProgress", value: create(WUpdateProgressSchema, {
        requestId: f.request_id,
        jobId: f.job_id,
        sequence: BigInt(f.sequence),
        phase: f.phase,
        message: f.message,
        terminal: f.terminal,
        success: f.success,
        error: f.error ?? "",
      })}});
  }
}

/** Build the typed worker binary frame. The caller supplies structured fields
 * directly; the old private 11-byte header and immediate decode pass were pure
 * construct/reparse work on every PTY chunk. */
export function binaryFrameToProto(
  channelId: number,
  direction: number,
  endSeq: number,
  data: Uint8Array,
): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "binary", value: create(WBinarySchema, {
      channelId,
      direction,
      seq: BigInt(endSeq),
      data,
    })},
  });
}
