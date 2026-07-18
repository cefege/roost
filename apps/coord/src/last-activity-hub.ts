// last-activity-hub — coord-authoritative last-activity timestamp per session.
//
// Stamps "last saw PTY output" (ms) per session from the SAME bytes coord
// already relays (globalBytesBus), and publishes it to lastActivityBus →
// the Sync stream fans it to every browser. Drives the sidebar "Last activity"
// filter: an OPEN session whose last activity is older than the window ages
// out (a long-idle terminal stops cluttering the list).
//
// Mirrors terminal-title-hub (one per coord, started from createCoord), but
// needs no parsing — every byte IS activity. Published THROTTLED (not per byte:
// claude redraws fire thousands of chunks) — the filter granularity is days,
// so ~once/minute is plenty. Snapshot seeds fresh Sync subscribers so a page
// load knows every session's last activity immediately (the bus is volatile).
//
// "We don't care for shells" (Author 2026-06-27): this stamps ALL sessions, but
// the consumer only ages out OPEN sessions, and claude sessions are the ones
// that matter — they get accurate timestamps because their bytes flow here.
//
// Depends on: buses (globalBytesBus in, lastActivityBus out, sessionBus reap).

import { diag } from "@roost/shared";
import { globalBytesBus, sessionBus, lastActivityBus } from "./buses.ts";

// Don't fan a frame on every byte — coalesce to at most one publish per session
// per window. Days-granularity filter → a minute of slack is invisible.
const THROTTLE_MS = 60_000;

interface Entry {
  lastTs: number;          // last byte seen (ms) — the snapshot value
  lastPublishedTs: number; // last value fanned to subscribers (throttle gate)
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

export function startLastActivityHub(): () => void {
  const unsubBytes = globalBytesBus.subscribe(({ session_id, bytes }) => {
    if (bytes.byteLength === 0) return;
    const now = Date.now();
    let e = _entries.get(session_id);
    if (!e) {
      // First byte for this session: publish immediately so a session that
      // just woke from idle reflects right away (not up to THROTTLE_MS late).
      e = { lastTs: now, lastPublishedTs: now };
      _entries.set(session_id, e);
      lastActivityBus.publish({ session_id, ts_ms: now });
      return;
    }
    e.lastTs = now;
    if (now - e.lastPublishedTs >= THROTTLE_MS) {
      e.lastPublishedTs = now;
      lastActivityBus.publish({ session_id, ts_ms: now });
      diag("last_activity.publish", { sid: session_id, ts_ms: now });
    }
  });

  // Drop a session's timestamp when it closes (mirrors terminal-title-hub).
  const unsubSessions = sessionBus.subscribe((ev) => {
    if (ev.kind !== "closed") return;
    _entries.delete(ev.session_id);
  });

  return () => {
    unsubBytes();
    unsubSessions();
    _entries.clear();
  };
}
