// Mecatl relay frames for the worker link: one opaque HTTP exchange out, one
// cancellation. Re-exported through worker-send.ts and called only by
// mecatl-relay.ts. Headers arrive already filtered; the daemon bearer is added
// on the worker, so no coordinator caller ever holds a Mecatl credential.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DMecatlRelayCancelSchema,
  DMecatlRelayRequestSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { currentRoutableWorker } from "./worker-send-target.ts";

/** Relay one browser→Mecatl HTTP exchange to the worker that owns the daemon.
 *  Returns false if the worker isn't connected. */
export function sendMecatlRelayRequest(
  workerFp: string,
  request: { requestId: string; method: string; path: string; headersJson: string; body: Uint8Array },
): boolean {
  const w = currentRoutableWorker(workerFp);
  if (!w) return false;
  try {
    const sent = w.send(create(CoordWorkerDownSchema, {
      frame: { case: "mecatlRelayRequest", value: create(DMecatlRelayRequestSchema, {
        requestId: request.requestId, method: request.method, path: request.path,
        headersJson: request.headersJson, body: request.body,
      })},
    }));
    return sent !== 0;
  } catch { return false; }
}

/** Abandon one in-flight Mecatl relay. The worker aborts its upstream fetch;
 *  an unknown request id is a no-op there, so a disconnected worker needs no
 *  retry. Returns false if the worker isn't connected. */
export function sendMecatlRelayCancel(workerFp: string, requestId: string): boolean {
  const w = currentRoutableWorker(workerFp);
  if (!w) return false;
  try {
    const sent = w.send(create(CoordWorkerDownSchema, {
      frame: { case: "mecatlRelayCancel", value: create(DMecatlRelayCancelSchema, { requestId }) },
    }));
    return sent !== 0;
  } catch { return false; }
}
