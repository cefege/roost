// Pure frame codecs for coord-link.ts — stateless conversions between the
// caller-facing UpstreamFrame / binary layout and wire CoordWorkerUp protos.
// Extracted to keep coord-link.ts under the 400-line cap; none of these close
// over the factory's mutable state, so they take all inputs as parameters.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WHelloSchema, WPongSchema, WBinarySchema,
  WTerminalMetadataSchema, WRpcOkSchema, WRpcErrorSchema,
  WInputResultSchema, WTerminalStreamResultSchema, WUpdateProgressSchema,
  WTerminalViewProjectionSchema, WTerminalViewStateSchema,
  WTerminalInputRouteResultSchema, WTerminalTransportProbeResultSchema,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import type { TerminalViewStateFrame } from "@roost/shared/proto/sync_pb";
import { PbTerminalViewInputSchema } from "@roost/shared/proto/wire_pb";
import type {
  TerminalMetadataFrame, TerminalViewProjectionFrame, UpstreamFrame,
} from "./coord-link-types.ts";

export function frameToProto(f: UpstreamFrame): CoordWorkerUp | null {
  switch (f.kind) {
    case "hello":
      return create(CoordWorkerUpSchema, { frame: { case: "hello", value: create(WHelloSchema, {
        workerFp: f.worker_fp, version: f.version, capabilities: [...(f.capabilities ?? [])],
      }) }});
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
    case "terminal-stream-result":
      return create(CoordWorkerUpSchema, { frame: { case: "terminalStreamResult", value: create(WTerminalStreamResultSchema, {
        requestId: f.request_id,
        sessionId: f.session_id,
        streamId: f.stream_id,
        enabled: f.enabled,
        status: f.status,
        channelResizeSeq: f.channel_resize_seq,
        effectiveCols: f.effective_cols,
        effectiveRows: f.effective_rows,
        resized: f.resized,
        reason: f.reason ?? "",
        phase: f.phase,
        failureKind: f.failure_kind,
      })}});
    case "local-terminal-peer-answer":
      return create(CoordWorkerUpSchema, {
        frame: { case: "localTerminalPeerAnswer", value: f.answer },
      });
    case "local-terminal-peer-error":
      return create(CoordWorkerUpSchema, {
        frame: { case: "localTerminalPeerError", value: f.error },
      });
    case "local-attachment-peer-answer":
      return create(CoordWorkerUpSchema, {
        frame: { case: "localAttachmentPeerAnswer", value: f.answer },
      });
    case "local-attachment-peer-error":
      return create(CoordWorkerUpSchema, {
        frame: { case: "localAttachmentPeerError", value: f.error },
      });
    case "attachment-direct-status":
      return create(CoordWorkerUpSchema, {
        frame: { case: "attachmentDirectStatus", value: f.status },
      });
    case "terminal-input-route-result":
      return create(CoordWorkerUpSchema, {
        frame: { case: "terminalInputRouteResult", value: create(WTerminalInputRouteResultSchema, {
          requestId: f.request_id,
          result: f.result,
        }) },
      });
    case "terminal-transport-probe-result":
      return create(CoordWorkerUpSchema, {
        frame: { case: "terminalTransportProbeResult", value: create(WTerminalTransportProbeResultSchema, {
          requestId: f.result.requestId,
          workerEpoch: f.result.workerEpoch,
        }) },
      });
    case "terminal-pipeline-snapshot":
      return create(CoordWorkerUpSchema, {
        frame: { case: "terminalPipelineSnapshot", value: f.snapshot },
      });
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

/** Build one compact semantic metadata frame without retaining PTY bytes. */
export function terminalMetadataFrameToProto(metadata: TerminalMetadataFrame): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "terminalMetadata", value: create(WTerminalMetadataSchema, {
      channelId: metadata.channelId,
      titleChanged: metadata.titleChanged,
      title: metadata.title,
      activityChanged: metadata.activityChanged,
      activityTsMs: BigInt(metadata.activityTsMs),
    })},
  });
}

/** Address one worker-owned view decision back to the browser socket the
 * coordinator relayed its command from. */
export function terminalViewStateToProto(
  socketId: string,
  frame: TerminalViewStateFrame,
): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "terminalViewState", value: create(WTerminalViewStateSchema, {
      socketId,
      frame,
    })},
  });
}

/** Publish one session's whole viewer membership, so the coordinator can
 * answer presence and diagnostics without owning it. */
export function terminalViewProjectionToProto(
  projection: TerminalViewProjectionFrame,
): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: { case: "terminalViewProjection", value: create(WTerminalViewProjectionSchema, {
      sessionId: projection.sessionId,
      viewers: projection.viewers.map((viewer) => create(PbTerminalViewInputSchema, {
        fingerprint: viewer.fingerprint,
        viewId: viewer.viewId,
        cols: viewer.cols,
        rows: viewer.rows,
        parked: viewer.parked,
        constrains: viewer.constrains,
      })),
      effectiveCols: projection.effectiveCols,
      effectiveRows: projection.effectiveRows,
      streamId: projection.streamId,
    })},
  });
}
