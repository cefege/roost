// Pure projection math over terminal view records and tombstones — no I/O, no
// socket access. Tombstones keep a parked view's revision/intent alive so an
// offline viewer can reclaim it; they are capped (128 per viewer, 131072 per
// process) and evicted LRU, meaning a very stale client's revision guard can
// be evicted. Effective geometry is the MINIMUM across the records that
// terminalViewConstrains() admits — the ONE definition of that membership.
import {
  TERMINAL_VIEW_LEASE_MS,
  TERMINAL_VIEW_PARK_GRACE_MS,
  minimumTerminalGeometry,
  type TerminalGeometry,
} from "../viewport.ts";
import type {
  TerminalViewIntent,
  TerminalViewStateSink,
} from "./terminal-view-protocol.ts";

const VIEWER_TOMBSTONE_CAP = 128;
const PROCESS_TOMBSTONE_CAP = 131_072;

export interface TerminalViewRecord extends TerminalViewIntent {
  key: string;
  viewId: string;
  viewerKey: string;
  fingerprint: string;
  socketId: string;
  revision: bigint;
  deadline: number;
  parked: boolean;
  /** When the owning socket dropped; 0 while the record is live. */
  parkedAt: number;
  /** Last observed terminalViewConstrains() value, written only from that
   * predicate. The sweep compares against it so a grace lapse re-minimizes on
   * exactly one tick instead of re-driving presence every second. */
  constrains: boolean;
}

export interface TerminalViewSocketRecord {
  id: string;
  viewerKey: string | null;
  fingerprint: string;
  allowsSession(sessionId: string): boolean;
  sink: TerminalViewStateSink;
  views: Set<string>;
}

export interface TerminalViewTombstone {
  key: string;
  viewerKey: string;
  revision: bigint;
  intent: TerminalViewIntent;
  expires: number;
}

/** Geometry decision input for one session: `live` are the records that
 * currently constrain the PTY, `retained` is every record still in membership
 * (a parked record past grace is retained but not live). */
export interface TerminalViewGeometrySet {
  live: readonly TerminalGeometry[];
  retained: number;
}

/** Per-viewer diagnostic row: what each record asked for and whether it is
 * part of the set that produced the session's effective geometry. */
export interface TerminalViewInput {
  fingerprint: string;
  viewId: string;
  cols: number;
  rows: number;
  parked: boolean;
  constrains: boolean;
}

export function activeTerminalFingerprints(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
): ReadonlySet<string> {
  const result = new Set<string>();
  for (const key of keys ?? []) {
    const view = views.get(key);
    if (view) result.add(view.fingerprint);
  }
  return result;
}

export function projectTerminalViewers(
  sessionViews: ReadonlyMap<string, ReadonlySet<string>>,
  views: ReadonlyMap<string, TerminalViewRecord>,
): ReadonlyMap<string, ReadonlyMap<string, TerminalGeometry>> {
  const result = new Map<string, ReadonlyMap<string, TerminalGeometry>>();
  for (const [sessionId, keys] of sessionViews) {
    const grouped = new Map<string, TerminalGeometry[]>();
    for (const key of keys) {
      const view = views.get(key);
      if (!view) continue;
      const geometries = grouped.get(view.fingerprint) ?? [];
      geometries.push(view);
      grouped.set(view.fingerprint, geometries);
    }
    const viewers = new Map<string, TerminalGeometry>();
    for (const [fingerprint, geometries] of grouped) {
      const geometry = minimumTerminalGeometry(geometries);
      if (geometry) viewers.set(fingerprint, geometry);
    }
    if (viewers.size) result.set(sessionId, viewers);
  }
  return result;
}

/** THE membership rule for effective geometry: a record constrains the PTY
 * while its lease holds and its socket is either live or inside the park
 * grace. Park absorbs reconnect wobble for reclaim (a stream-continuity
 * question); it must not pin everyone else's PTY to a viewer whose socket is
 * gone. Every aggregation of viewer geometry goes through this predicate. */
export function terminalViewConstrains(
  view: TerminalViewRecord,
  now: number,
): boolean {
  if (view.deadline <= now) return false;
  return !view.parked || now < view.parkedAt + TERMINAL_VIEW_PARK_GRACE_MS;
}

export function terminalViewGeometrySet(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
  now: number,
): TerminalViewGeometrySet {
  const live: TerminalGeometry[] = [];
  let retained = 0;
  for (const key of keys ?? []) {
    const view = views.get(key);
    if (!view) continue;
    retained += 1;
    if (terminalViewConstrains(view, now)) live.push(view);
  }
  return { live, retained };
}

export function projectTerminalViewInputs(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
  now: number,
): readonly TerminalViewInput[] {
  const result: TerminalViewInput[] = [];
  for (const key of keys ?? []) {
    const view = views.get(key);
    if (!view) continue;
    result.push({
      fingerprint: view.fingerprint,
      viewId: view.viewId,
      cols: view.cols,
      rows: view.rows,
      parked: view.parked,
      constrains: terminalViewConstrains(view, now),
    });
  }
  return result;
}

export function terminalViewStats(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
): { activeViews: number; parkedViews: number } {
  let activeViews = 0;
  let parkedViews = 0;
  for (const key of keys ?? []) {
    const view = views.get(key);
    if (!view) continue;
    if (view.parked) parkedViews += 1;
    else activeViews += 1;
  }
  return { activeViews, parkedViews };
}

export function retainTerminalViewTombstone(
  tombstones: Map<string, TerminalViewTombstone>,
  now: number,
  key: string,
  viewerKey: string,
  revision: bigint,
  intent: TerminalViewIntent,
): void {
  tombstones.delete(key);
  tombstones.set(key, {
    key,
    viewerKey,
    revision,
    intent: { ...intent },
    expires: now + TERMINAL_VIEW_LEASE_MS,
  });
  const own = [...tombstones.values()].filter((entry) => entry.viewerKey === viewerKey);
  while (own.length > VIEWER_TOMBSTONE_CAP) {
    const oldest = own.shift();
    if (oldest) tombstones.delete(oldest.key);
  }
  while (tombstones.size > PROCESS_TOMBSTONE_CAP) {
    const oldest = tombstones.keys().next().value as string | undefined;
    if (!oldest) break;
    tombstones.delete(oldest);
  }
}
