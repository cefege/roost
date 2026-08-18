// Barrier-repair full-frame replay. The announcement barrier can drop cell
// frames for one exact (worker, session, channel) route; recovery replays a
// HEARTBEAT-shaped viewport claim for the viewers already watching, on the same
// per-viewer lane as browser intents. Split out of session-control.ts, which is
// now a re-export barrel. Sole caller: worker-ws-handler.ts.

import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { subscribedCellSeq } from "./cell-subscriptions.ts";
import { _viewersBySession } from "./viewer-tracker.ts";
import { sendTerminalViewportRequest } from "./worker-send.ts";
import { enqueueLane } from "./terminal-control-lane.ts";

export interface BarrierRepairRoute {
  workerFp: string;
  sessionId: string;
  channelId: number;
}

export interface BarrierRepairReplay {
  /** Refreshes enqueued; 0 = nobody is watching this session yet. */
  enqueued: number;
  /** Settles once every enqueued refresh has left the coordinator lane. */
  settled: Promise<void>;
}

/** The announcement barrier lost cell frames for one exact route. Replay a
 *  HEARTBEAT-shaped claim for every viewer that is actually watching, at the
 *  watermark the coordinator already installed and with `held_cell_seq = 0`: the
 *  worker's stale-sequence path answers with one authoritative full frame, so
 *  recovery never waits for an unrelated delta or a browser reload. With nobody
 *  watching, the standing repair mark alone covers that tab's next claim.
 *
 *  Membership, geometry, and ordering stay untouched — the refresh runs on the
 *  same per-viewer lane as browser intents, carries no new intent, and never
 *  advances the browser's watermark. Background panes at 0×0 are skipped: a
 *  positive-cause claim would read as a withdraw, and their own next visible
 *  claim picks up the override. No write lease is taken because nothing here
 *  mutates coordinator state. */
export function requestBarrierRepairFullFrame(route: BarrierRepairRoute): BarrierRepairReplay {
  const viewers = _viewersBySession.get(route.sessionId);
  if (!viewers) return { enqueued: 0, settled: Promise.resolve() };
  const replays: Array<Promise<unknown>> = [];
  for (const viewerKey of viewers.keys()) {
    const clientSeq = subscribedCellSeq(viewerKey, route.sessionId);
    if (clientSeq === null) continue;
    const geometry = viewers.get(viewerKey);
    if (!geometry || geometry.cols <= 0 || geometry.rows <= 0) continue;
    replays.push(enqueueLane(
      viewerKey,
      route.sessionId,
      0,
      async (releaseLane) => {
        // Re-read at send time: a withdraw or resize may have overtaken the
        // drop while this refresh waited for the lane.
        const current = _viewersBySession.get(route.sessionId)?.get(viewerKey);
        const seq = subscribedCellSeq(viewerKey, route.sessionId);
        if (!current || seq === null || current.cols <= 0 || current.rows <= 0) {
          releaseLane();
          return false;
        }
        const request = sendTerminalViewportRequest(route.workerFp, {
          sessionId: route.sessionId,
          viewerId: viewerKey,
          clientSeq: seq,
          cols: current.cols,
          rows: current.rows,
          cause: ResizeCause.HEARTBEAT,
          heldCellSeq: 0n,
        });
        releaseLane();
        // The refresh carries no intent, so its only product is the worker's
        // full frame: a lost result changes nothing, and the standing mark keeps
        // the override until that frame actually publishes.
        void request.result.catch(() => undefined);
        return request.admitted;
      },
      () => false,
    ).catch(() => undefined));
  }
  return {
    enqueued: replays.length,
    settled: Promise.all(replays).then(() => undefined),
  };
}
