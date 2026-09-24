// Presence fan-out. Worker upstream `presence` frames (CoordWorkerUpstream)
// carry an opaque `payload`; publishPresence re-keys them by session_id onto
// the globalPresenceBus firehose the SPA's single Sync sub consumes.
//
// Producer: handlers-sessions.ts imports publishPresence dynamically when a
// worker relays a presence frame, so this file has no static importer — grep
// for the dynamic import, not a top-level one, when tracing the call path.

import { globalPresenceBus } from "../events/buses.ts";
import { lookupSessionId } from "../terminal/screen/byte-hub.ts";
import { asWorkerFp, asChannelId } from "@roost/protocol/wire";
import { log } from "@roost/observability/log";

export function publishPresence(workerFp: string, channelId: number, payload: unknown): void {
  // phase-26 firehose: publish keyed by session_id so the SPA's single
  // firehose sub receives presence for every session without one
  // sessions.presence EventSource per Terminal.
  const sessionId = lookupSessionId(asWorkerFp(workerFp), asChannelId(channelId));
  if (sessionId) globalPresenceBus.publish({ session_id: sessionId, data: payload });
  log.debug("presence-hub", "published", { workerFp, channelId, sessionId });
}
