// Presence fan-out. Worker upstream `presence` frames (CoordWorkerUpstream)
// carry an opaque `payload`; publishPresence re-keys them by session_id onto
// the globalPresenceBus firehose the SPA's single Sync sub consumes.
//
// No current producer (worker stopped emitting presence when its inbound WS
// surface was removed); kept as the forward-compat wire path for multi-viewer.

import { globalPresenceBus } from "./buses.ts";
import { lookupSessionId } from "./byte-hub.ts";
import { asWorkerFp, asChannelId } from "@roost/shared/wire";
import { log } from "@roost/shared/log";

export function publishPresence(workerFp: string, channelId: number, payload: unknown): void {
  // phase-26 firehose: publish keyed by session_id so the SPA's single
  // firehose sub receives presence for every session without one
  // sessions.presence EventSource per Terminal.
  const sessionId = lookupSessionId(asWorkerFp(workerFp), asChannelId(channelId));
  if (sessionId) globalPresenceBus.publish({ session_id: sessionId, data: payload });
  log.debug("presence-hub", "published", { workerFp, channelId, sessionId });
}
