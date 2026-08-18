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
// applySessionsSnapshot shares those per-key writes with the bootstrap /
// re-hydration path, so a full session set always lands the same way.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import type { SessionEvent, Session } from "@roost/shared/wire";
import { foldEvent, SessionEvent as SessionEventSchema } from "@roost/shared/wire";
import { signal } from "@roost/shared/diag";
import { rootStore, setRootStore } from "./root.ts";
import { isPendingSpawn } from "./optimisticSpawn.ts";
import { pruneCellFrameCount } from "./sync-dispatch.ts";
import {
  noteTerminalProducerGeneration,
  pruneTerminalOutbound,
} from "../ws/sync-outbound.ts";
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

/** Drop a session's store entry plus every volatile slice keyed by its id —
 *  they have no other reaper, so each closed session would otherwise leak one
 *  entry for the life of the tab (the days-long-uptime bloat). Never deletes an
 *  in-flight optimistic placeholder: a `snapshot` for its worker_fp, or a full
 *  re-hydration, arriving mid-spawn omits the not-yet-real session id. */
function _deleteSession(id: string): void {
  if (isPendingSpawn(id)) return;
  setRootStore("sessions", id, undefined as unknown as Session);
  setRootStore("terminal_title", id, undefined as unknown as string);
  setRootStore("last_activity", id, undefined as unknown as never);
  setRootStore("session_viewers", id, undefined as unknown as never);
  clearAgentStatusForSession(id);
  pruneCellFrameCount(id); // module-private Map, same per-session-reaper duty
  pruneTerminalOutbound(id); // input/viewport queues + persisted intent
  pruneSessionTrace(id); // diag session_trace_id cache
}

/** Upsert one session by id. An existing record is RECONCILED, never replaced:
 *  folds and snapshots both carry complete objects, but only changed leaves may
 *  notify, and the store proxy this session's mounted terminal reads must
 *  survive a metadata refresh. New sessions take the plain set. */
function _upsertSession(id: string, next: Session): void {
  if (rootStore.sessions[id]) setRootStore("sessions", id, reconcile(next));
  else setRootStore("sessions", id, next);
}

/** Apply an authoritative full session set — bootstrap hydrate, and every
 *  re-hydration a reconnect's fresh domain generation triggers. Same convention
 *  as the event fold: per-id reconcile, explicit deletion of absent
 *  non-optimistic sessions, identical volatile-slice cleanup. A whole-record
 *  `setRootStore("sessions", rec)` cannot do this — Solid merges it key by key,
 *  so it replaces every session object (invalidating every subscriber) and
 *  prunes nothing, leaving closed sessions in the sidebar until a reload. */
export function applySessionsSnapshot(sessions: Record<string, Session>): void {
  batch(() => {
    for (const id of Object.keys(rootStore.sessions)) {
      if (!(id in sessions)) _deleteSession(id);
    }
    for (const id of Object.keys(sessions)) _upsertSession(id, sessions[id]!);
  });
}

export function foldEventIntoStore(event: SessionEvent): void {
  // Validate at the boundary. The Sync stream hands this layer a decoded but
  // UNVERIFIED event; a Zod-rejected payload must never reach the store,
  // because a fold that throws part-way through the batch below would leave a
  // live session's slices unwound with no event having said it closed.
  const parsed = SessionEventSchema.safeParse(event);
  if (!parsed.success) {
    const rejected: Record<string, unknown> = event;
    const sid = typeof rejected.session_id === "string" ? rejected.session_id : undefined;
    signal("diag.corruption_signal", {
      kind: "projector_event_rejected",
      sid,
      event_kind: String(rejected.kind),
      msg: parsed.error.issues[0]?.message ?? "session event failed validation",
      cooldownKey: sid,
    });
    return;
  }
  const valid = parsed.data;
  try {
  const ids = affectedIds(valid);
  const prev = new Map<string, Session>();
  for (const id of ids) {
    const s = rootStore.sessions[id];
    if (s) prev.set(id, s as Session);
  }
  const next = foldEvent(prev, valid);
  // Diff into the Solid store, per key. foldEvent returns new object refs
  // only for entries it changed, so `prev.get(id) !== s` writes exactly
  // the changed sessions. All writes for one event flush in ONE batch — a
  // snapshot's K upserts / a deletion's 3 slice-drops no longer trigger K
  // separate downstream recomputes.
  batch(() => {
    // `closed` is the only event that removes a session. An absent session in a
    // stale or reordered `snapshot` is an offline breadcrumb, not a deletion, so
    // unwinding a live session can never be a byproduct of an out-of-order fold
    // (the authoritative full set still prunes through applySessionsSnapshot).
    if (valid.kind === "closed") {
      for (const id of prev.keys()) {
        if (!next.has(id)) _deleteSession(id);
      }
    }
    for (const [id, s] of next) {
      if (prev.get(id) === s) continue;
      _upsertSession(id, s);
    }
  });
  } catch (e) {
    // A fold throw must NOT unwind into the sync stream (it would be mislabeled
    // a network drop → fake reconnect churn). Signal + swallow.
    const sid = "session_id" in valid ? valid.session_id : undefined;
    signal("diag.corruption_signal", {
      kind: "projector_fold_throw",
      sid,
      event_kind: valid.kind,
      msg: String(e),
      cooldownKey: sid,
    });
    return;
  }
  // Producer generation: a respawn hands the session a new keeper core, and a
  // worker boot/reconcile snapshot means the announcing worker lost the claims
  // it held. The projection now agrees with the coordinator's route, so every
  // tab holding a current positive viewport owner replays a newer claim that
  // demands the new core's first full frame; inactive tabs do nothing.
  if (valid.kind === "respawned") noteTerminalProducerGeneration([valid.session_id]);
  else if (valid.kind === "snapshot") {
    noteTerminalProducerGeneration(valid.sessions.map((session) => session.id));
  }
}
