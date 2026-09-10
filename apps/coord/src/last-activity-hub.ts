// Coordinator-owned retained terminal activity timestamps.
// The worker transport adapter supplies semantic activity observations; this
// hub preserves first-publication and local 60-second fan-out throttling.
// Session close events release retained state.

import { diag } from "@roost/shared/diag";
import { TERMINAL_METADATA_ACTIVITY_THROTTLE_MS } from "@roost/shared/terminal-metadata";
import { sessionBus, lastActivityBus } from "./buses.ts";

// Do not fan a frame on every semantic activity observation. Local receipt
// time gates publication so worker clock differences cannot alter the bound.
export const LAST_ACTIVITY_THROTTLE_MS = TERMINAL_METADATA_ACTIVITY_THROTTLE_MS;

interface Entry {
  lastTs: number;
  lastPublishedAtMs: number;
}

const _entries = new Map<string, Entry>();

/** Current last-activity ms per session — replayed to each new Sync subscriber
 *  so a fresh page load can age out idle sessions immediately (lastActivityBus
 *  is throttled/volatile, not backfilled). */
export function getLastActivitySnapshot(): Array<{ session_id: string; ts_ms: number }> {
  const out: Array<{ session_id: string; ts_ms: number }> = [];
  for (const [sid, e] of _entries) out.push({ session_id: sid, ts_ms: e.lastTs });
  return out;
}

/** Accept one semantic terminal activity observation from a worker route. */
export function observeTerminalActivity(sessionId: string, observedAtMs: number): void {
  const receivedAtMs = Date.now();
  const timestamp = Number.isSafeInteger(observedAtMs) && observedAtMs >= 0
    ? observedAtMs
    : receivedAtMs;
  let entry = _entries.get(sessionId);
  if (!entry) {
    entry = { lastTs: timestamp, lastPublishedAtMs: receivedAtMs };
    _entries.set(sessionId, entry);
    lastActivityBus.publish({ session_id: sessionId, ts_ms: timestamp });
    return;
  }
  entry.lastTs = timestamp;
  if (receivedAtMs - entry.lastPublishedAtMs < LAST_ACTIVITY_THROTTLE_MS) return;
  entry.lastPublishedAtMs = receivedAtMs;
  lastActivityBus.publish({ session_id: sessionId, ts_ms: timestamp });
  diag("last_activity.publish", { sid: sessionId, ts_ms: timestamp });
}

export function startLastActivityHub(): () => void {
  const unsubSessions = sessionBus.subscribe((event) => {
    if (event.kind === "closed") _entries.delete(event.session_id);
  });
  return () => {
    unsubSessions();
    _entries.clear();
  };
}
