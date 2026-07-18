// Outbound worker frames: the socket-shape shim the router/files/scrollback
// use, plus the browser-command and attachment-chunk senders. Each resolves
// the target worker through the shared connectWorkers registry and writes a
// CoordWorkerDown frame on its live send handle.

import { create } from "@bufbuild/protobuf";
import { CoordWorkerDownSchema, DBrowserCommandSchema, DBinarySchema, DAttachmentChunkSchema } from "@roost/shared/proto/worker_transport_pb";
import { connectWorkers } from "./worker-registry.ts";

/** Socket-shape shim: presents the worker-conn registry to call sites
 * as a `.send(string|Uint8Array)` handle so router.ts/files.ts/scrollback
 * don't have to know which transport is underneath. */
export function getWorkerHubSocket(workerFp: string): { send(data: string | Uint8Array): void } | null {
  const w = connectWorkers.get(workerFp);
  if (!w) return null;
  return {
    send(data: string | Uint8Array): void {
      if (typeof data === "string") {
        // browser-command JSON envelope: { kind: "browser-command", browser_id, viewer_id, request_id, frame }
        const parsed = JSON.parse(data) as {
          kind: "browser-command";
          browser_id: string; viewer_id: string; request_id: string; frame: unknown;
        };
        w.send(create(CoordWorkerDownSchema, {
          frame: { case: "browserCommand", value: create(DBrowserCommandSchema, {
            browserId: parsed.browser_id,
            viewerId: parsed.viewer_id,
            requestId: parsed.request_id,
            frameJson: JSON.stringify(parsed.frame),
          })},
        }));
      } else {
        // Raw binary frame: [2-byte BE channel][1-byte direction][payload]
        if (data.length < 3) return;
        const channelId = (data[0]! << 8) | data[1]!;
        const direction = data[2]!;
        const payload = data.subarray(3);
        w.send(create(CoordWorkerDownSchema, {
          frame: { case: "binary", value: create(DBinarySchema, {
            channelId, direction, data: payload,
          })},
        }));
      }
    },
  };
}

/** Send a JSON-encoded ClientControlFrame to a worker as a browser-command.
 * Returns true if the worker is currently connected and the frame was
 * queued for delivery. */
export function sendBrowserCommand(
  workerFp: string,
  msg: { browser_id: string; viewer_id: string; request_id: string; frame: unknown },
): boolean {
  const w = connectWorkers.get(workerFp);
  if (!w) return false;
  try {
    w.send(create(CoordWorkerDownSchema, {
      frame: { case: "browserCommand", value: create(DBrowserCommandSchema, {
        browserId: msg.browser_id,
        viewerId: msg.viewer_id,
        requestId: msg.request_id,
        frameJson: JSON.stringify(msg.frame),
      })},
    }));
    return true;
  } catch { return false; }
}

/** att1-stream — relay one streamed-upload chunk to a worker. The first chunk
 *  carries metadata; every chunk carries `data`; `last` triggers rename+reply.
 *  Returns false if the worker isn't connected. */
export function sendAttachmentChunk(
  workerFp: string,
  chunk: { requestId: string; sessionId: string; filename: string; shortPath: boolean; data: Uint8Array; last: boolean; seq: number },
): boolean {
  const w = connectWorkers.get(workerFp);
  if (!w) return false;
  try {
    w.send(create(CoordWorkerDownSchema, {
      frame: { case: "attachmentChunk", value: create(DAttachmentChunkSchema, {
        requestId: chunk.requestId, sessionId: chunk.sessionId, filename: chunk.filename,
        shortPath: chunk.shortPath, data: chunk.data, last: chunk.last, seq: chunk.seq,
      })},
    }));
    return true;
  } catch { return false; }
}
