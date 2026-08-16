// Projection layer: folds a SessionEvent into the root store.sessions record.
//
// SINGLE SOURCE OF TRUTH: this delegates the fold DECISION to the SAME
// `foldEvent` coord uses (@roost/shared/wire). There is NO hand-mirrored
// switch here anymore — the prior parallel copy drifted (it silently
// dropped the `respawned` variant), which is the whole projection-drift
// bug class. By construction now, coord's DB projection and the SPA's
// in-memory projection cannot disagree: same function, same event.
//
// The only SPA-specific concern is reactivity: foldEvent works on a plain
// Map; we apply its result to the Solid store via PER-KEY writes (a whole-
// Record setStore silently no-ops on a subtree — feedback_solid_setstore_record_replace).
// We build the affected slice of the map, fold, then diff per session_id.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import type { SessionEvent, Session } from "@roost/shared/wire";
import { foldEvent } from "@roost/shared/wire";
import { signal } from "@roost/shared/diag";
import { rootStore, setRootStore } from "./root.ts";
import { isPendingSpawn } from "./optimisticSpawn.ts";
import { pruneCellFrameCount } from "./sync-dispatch.ts";
import { pruneTerminalOutbound } from "../ws/sync-outbound.ts";
import { pruneSessionTrace } from "../lib/diag.ts";
import { clearAgentStatusForSession } from "./agent-status.ts";

/** session_ids whose store entry this event could change — the slice we
 *  must hand foldEvent so its result is correct. snapshot replaces every
 *  session of one worker (current + announced); every other variant
 *  touches exactly its own session_id. */
function affectedIds(event: SessionEvent): Set<string> {
  if (event.kind === "snapshot") {
    const ids = new Set<string>();
    for (const [id, s] of Object.entries(rootStore.sessions)) {
      if (s.worker_fp === event.worker_fp) ids.add(id);
    }
    for (const s of event.sessions) ids.add(s.id);
    return ids;
  }
  // Every remaining variant carries a session_id (attached/detached fold
  // to a no-op, harmless to include).
  return new Set<string>([(event as { session_id: string }).session_id]);
}

export function foldEventIntoStore(event: SessionEvent): void {
  try {
  const ids = affectedIds(event);
  const prev = new Map<string, Session>();
  for (const id of ids) {
    const s = rootStore.sessions[id];
    if (s) prev.set(id, s as Session);
  }
  const next = foldEvent(prev, event);
  // Diff into the Solid store, per key. foldEvent returns new object refs
  // only for entries it changed, so `prev.get(id) !== s` writes exactly
  // the changed sessions; entries gone from `next` are real deletions
  // (closed / snapshot-stale). All writes for one event flush in ONE
  // batch — a snapshot's K upserts / a deletion's 3 slice-drops no longer
  // trigger K separate downstream recomputes.
  batch(() => {
    for (const id of prev.keys()) {
      if (!next.has(id)) {
        // Never delete an in-flight optimistic placeholder: a `snapshot` delta for
        // its worker_fp arriving mid-spawn omits the not-yet-real session id.
        if (isPendingSpawn(id)) continue;
        setRootStore("sessions", id, undefined as unknown as Session);
        // Drop the session's volatile coord-streamed slices too — they're keyed
        // by session id and have no other reaper, so they'd leak one entry per
        // closed session for the life of the tab (the days-long-uptime bloat).
        setRootStore("terminal_title", id, undefined as unknown as string);
        setRootStore("last_activity", id, undefined as unknown as never);
        setRootStore("session_viewers", id, undefined as unknown as never);
        clearAgentStatusForSession(id);
        pruneCellFrameCount(id); // module-private Map, same per-session-reaper duty
        pruneTerminalOutbound(id); // input/viewport queues + persisted intent
        pruneSessionTrace(id); // diag session_trace_id cache
      }
    }
    for (const [id, s] of next) {
      if (prev.get(id) === s) continue;
      // Reconcile existing session records instead of replacing their proxies:
      // event folds return complete objects, but only changed leaves should
      // notify subscribers. New sessions can take the plain set.
      if (rootStore.sessions[id]) setRootStore("sessions", id, reconcile(s));
      else setRootStore("sessions", id, s);
    }
  });
  } catch (e) {
    // A foldEvent Zod-reject must NOT unwind into the sync stream (it would be
    // mislabeled a network drop → fake reconnect churn). Signal + swallow.
    const sid = "session_id" in event ? event.session_id : undefined;
    signal("diag.corruption_signal", {
      kind: "projector_fold_throw",
      sid,
      event_kind: event.kind,
      msg: String(e),
      cooldownKey: sid,
    });
  }
}
