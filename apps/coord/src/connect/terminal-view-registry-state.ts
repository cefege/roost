// Pure projection math over terminal view records and tombstones — no I/O, no
// socket access. Tombstones keep a parked view's revision/intent alive so an
// offline viewer can reclaim it; they are capped (128 per viewer, 131072 per
// process) and evicted LRU, meaning a very stale client's revision guard can
// be evicted. Effective geometry is the MINIMUM across all viewers.
import {
  TERMINAL_VIEW_LEASE_MS,
  minimumTerminalGeometry,
  type TerminalGeometry,
} from "@roost/shared/viewport";
import type { TerminalScreenSocketSink } from "./terminal-screen-hub.ts";
import type { TerminalViewIntent } from "./terminal-view-protocol.ts";

const VIEWER_TOMBSTONE_CAP = 128;
const PROCESS_TOMBSTONE_CAP = 131_072;

export interface TerminalViewRecord extends TerminalViewIntent {
  key: string;
  viewId: string;
  viewerKey: string;
  fingerprint: string;
  dashboardId: string;
  socketId: string;
  revision: bigint;
  deadline: number;
  parked: boolean;
}

export interface TerminalViewSocketRecord {
  id: string;
  viewerKey: string | null;
  fingerprint: string;
  dashboardId: string;
  allowsSession(sessionId: string): boolean;
  sink: TerminalScreenSocketSink;
  views: Set<string>;
}

export interface TerminalViewTombstone {
  key: string;
  viewerKey: string;
  revision: bigint;
  dashboardId: string;
  intent: TerminalViewIntent;
  expires: number;
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

export function terminalViewGeometries(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
): readonly TerminalGeometry[] {
  const result: TerminalGeometry[] = [];
  for (const key of keys ?? []) {
    const view = views.get(key);
    if (view) result.push(view);
  }
  return result;
}

export function terminalViewStats(
  keys: ReadonlySet<string> | undefined,
  views: ReadonlyMap<string, TerminalViewRecord>,
): { activeViews: number; parkedViews: number } {
  let parkedViews = 0;
  for (const key of keys ?? []) if (views.get(key)?.parked) parkedViews += 1;
  return { activeViews: keys?.size ?? 0, parkedViews };
}

export function retainTerminalViewTombstone(
  tombstones: Map<string, TerminalViewTombstone>,
  now: number,
  key: string,
  viewerKey: string,
  dashboardId: string,
  revision: bigint,
  intent: TerminalViewIntent,
): void {
  tombstones.delete(key);
  tombstones.set(key, {
    key,
    viewerKey,
    dashboardId,
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
