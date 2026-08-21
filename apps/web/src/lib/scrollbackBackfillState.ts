import { ScrollbackHistoryFloor as PbScrollbackHistoryFloor } from "@roost/shared/proto/coordinator_pb";
import type { ScrollbackHistoryFloor } from "@roost/shared/wire";

/** Per-session observability the diagnostic surfaces read without holding a
 * controller. One entry per session this document has backfilled — the same
 * bound as the plain request counter it replaces; the value simply carries the
 * proven floor alongside the count instead of adding a second map. */
export interface BackfillState {
  requests: number;
  /** Earliest absolute row the worker proved it still retains. 0 = never hit. */
  floor: number;
  /** Why that floor is there, straight off the response that established it. */
  floorReason: ScrollbackHistoryFloor;
}

const stateBySession = new Map<string, BackfillState>();

export function backfillStateOf(sessionId: string): BackfillState {
  let state = stateBySession.get(sessionId);
  if (state === undefined) {
    state = { requests: 0, floor: 0, floorReason: "none" };
    stateBySession.set(sessionId, state);
  }
  return state;
}

/** Smoke observability: issued epoch-addressed history RPCs per session. */
export function scrollbackBackfillRequestCount(sessionId: string): number {
  return stateBySession.get(sessionId)?.requests ?? 0;
}

/** The history floor this document has proven for a session and WHY it is there:
 * "evicted" = gone forever, "resize_replay" = a resize rebuilt the grid from the
 * worker's bounded byte ring and that ring could not reach as far back as the
 * core it replaced. null until a page actually comes back short, so a blank
 * top-of-history is attributable instead of merely visible. */
export function scrollbackHistoryFloor(
  sessionId: string,
): { row: number; reason: ScrollbackHistoryFloor } | null {
  const state = stateBySession.get(sessionId);
  if (state === undefined || state.floorReason === "none") return null;
  return { row: state.floor, reason: state.floorReason };
}

/** The get-scrollback-cells response's proto enum in the vocabulary the rest of
 * Roost speaks. Total over the enum, so a reason added to the proto cannot
 * silently read as "none"; the `??` at each use covers only a number no version
 * of the enum defines. Shared with the smoke retained-history pager, the other
 * client of this RPC, so the decode exists once. */
export const SCROLLBACK_FLOOR_REASON: Record<PbScrollbackHistoryFloor, ScrollbackHistoryFloor> = {
  [PbScrollbackHistoryFloor.UNSPECIFIED]: "none",
  [PbScrollbackHistoryFloor.EVICTED]: "evicted",
  [PbScrollbackHistoryFloor.RESIZE_REPLAY]: "resize_replay",
};
