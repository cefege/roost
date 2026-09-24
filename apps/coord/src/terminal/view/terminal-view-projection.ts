// Coordinator memory of the workers that own their own terminal views: which
// live connection advertised `terminal-view-owner-v1`, which sessions it owns,
// and the membership it publishes through WTerminalViewProjection. This is a
// read model only — no minimizer, no stream, no geometry decision — so the
// coordinator can still answer presence and diagnostics for those sessions.
// worker-conn.ts owns the registration lifetime; terminal-view-hub.ts reads it.

import type { WTerminalViewProjection } from "@roost/protocol/proto/worker_transport_pb";
import type { TerminalViewInput } from "@roost/protocol/terminal-view";
import { minimumTerminalGeometry, type TerminalGeometry } from "@roost/protocol/viewport";
import { getCachedSessionWorker } from "../screen/byte-hub.ts";

/** Advertised in WHello.capabilities and echoed in DHelloAck.capabilities.
 * The worker declares the same literal for its own hello. */
export const TERMINAL_VIEW_OWNER_CAPABILITY = "terminal-view-owner-v1";

/** Identity-stamped so a reconnecting worker's delayed old socket cannot
 * un-register the replacement connection's ownership. */
export interface TerminalViewOwnerRegistration {
  release(): void;
}

/** One owner-mode session's published membership. `effective` and `streamId`
 * are the worker's, never the coordinator's. */
export interface TerminalViewOwnerRow {
  readonly workerFp: string;
  readonly viewers: readonly TerminalViewInput[];
  readonly effective: TerminalGeometry | null;
  readonly streamId: string;
}

const owners = new Map<string, TerminalViewOwnerRegistration>();
const rows = new Map<string, TerminalViewOwnerRow>();
// sessionId → owning worker fingerprint. Survives the route-cache sweep a
// worker hello performs: without it a view heartbeat arriving between that
// hello and the worker's exact snapshot would look routeless, fall back to
// coordinator-owned membership, and become a second minimizer for a session
// the worker already owns.
const ownerSessions = new Map<string, string>();

export function registerTerminalViewOwner(workerFp: string): TerminalViewOwnerRegistration {
  const registration: TerminalViewOwnerRegistration = {
    release(): void {
      if (owners.get(workerFp) !== registration) return;
      dropTerminalViewOwner(workerFp);
    },
  };
  owners.set(workerFp, registration);
  return registration;
}

/** A hello WITHOUT the capability is authoritative for its fingerprint. The
 * superseded connection's identity-stamped release only runs when its socket
 * close event lands, which is strictly later, so a worker that downgraded
 * would otherwise keep having its sessions relayed to a build that no longer
 * speaks the relay and silently drops every view command. */
export function clearTerminalViewOwner(workerFp: string): void {
  if (owners.has(workerFp)) dropTerminalViewOwner(workerFp);
}

function dropTerminalViewOwner(workerFp: string): void {
  owners.delete(workerFp);
  for (const [sessionId, row] of rows) {
    if (row.workerFp === workerFp) rows.delete(sessionId);
  }
  for (const [sessionId, fp] of ownerSessions) {
    if (fp === workerFp) ownerSessions.delete(sessionId);
  }
}

/** The owner-mode worker that owns this session's terminal views, or null when
 * the session belongs to a legacy worker and the coordinator owns them. */
export function terminalViewOwnerForSession(sessionId: string): string | null {
  const cached = getCachedSessionWorker(sessionId);
  if (cached) {
    if (!owners.has(cached.worker_fp)) {
      ownerSessions.delete(sessionId);
      return null;
    }
    ownerSessions.set(sessionId, cached.worker_fp);
    return cached.worker_fp;
  }
  const remembered = ownerSessions.get(sessionId);
  if (remembered === undefined) return null;
  if (owners.has(remembered)) return remembered;
  ownerSessions.delete(sessionId);
  return null;
}

/** Replace a session's membership wholesale. The worker publishes the full
 * viewer list on every membership or effective-geometry change, including an
 * empty list when the last viewer goes inactive, so no incremental merge
 * exists. An empty list leaves a zero-viewer row rather than deleting it: the
 * coordinator-owned path keeps reporting a session whose views all went away
 * until the session itself closes, and diagnostics read that difference.
 */
export function applyTerminalViewProjection(
  workerFp: string,
  projection: WTerminalViewProjection,
): void {
  const sessionId = projection.sessionId;
  if (!sessionId || !owners.has(workerFp)) return;
  const known = getCachedSessionWorker(sessionId)?.worker_fp ?? ownerSessions.get(sessionId);
  if (known !== undefined && known !== workerFp) return;
  ownerSessions.set(sessionId, workerFp);
  rows.set(sessionId, {
    workerFp,
    viewers: projection.viewers.map((viewer) => ({
      fingerprint: viewer.fingerprint,
      viewId: viewer.viewId,
      cols: viewer.cols,
      rows: viewer.rows,
      parked: viewer.parked,
      constrains: viewer.constrains,
    })),
    effective: projection.effectiveCols > 0 && projection.effectiveRows > 0
      ? { cols: projection.effectiveCols, rows: projection.effectiveRows }
      : null,
    streamId: projection.streamId,
  });
}

export function dropTerminalViewProjection(sessionId: string): void {
  rows.delete(sessionId);
  ownerSessions.delete(sessionId);
}

export function terminalViewOwnerRow(sessionId: string): TerminalViewOwnerRow | null {
  return rows.get(sessionId) ?? null;
}

export function ownerTerminalViewerFingerprints(sessionId: string): ReadonlySet<string> | null {
  const row = rows.get(sessionId);
  if (!row) return null;
  const result = new Set<string>();
  for (const viewer of row.viewers) result.add(viewer.fingerprint);
  return result;
}

/** Per-device geometry for presence, matching the coordinator registry's own
 * projection: every retained record contributes, parked or not. */
export function ownerTerminalViewerGeometry(
  sessionId: string,
): ReadonlyMap<string, TerminalGeometry> | null {
  const row = rows.get(sessionId);
  if (!row) return null;
  const grouped = new Map<string, TerminalGeometry[]>();
  for (const viewer of row.viewers) {
    const geometries = grouped.get(viewer.fingerprint) ?? [];
    geometries.push(viewer);
    grouped.set(viewer.fingerprint, geometries);
  }
  const viewers = new Map<string, TerminalGeometry>();
  for (const [fingerprint, geometries] of grouped) {
    const geometry = minimumTerminalGeometry(geometries);
    if (geometry) viewers.set(fingerprint, geometry);
  }
  return viewers;
}

export function mergeOwnerTerminalViewerProjection(
  into: Map<string, ReadonlyMap<string, TerminalGeometry>>,
): void {
  for (const sessionId of rows.keys()) {
    const viewers = ownerTerminalViewerGeometry(sessionId);
    if (viewers?.size) into.set(sessionId, viewers);
  }
}

/** Test seam: these registries are process-global and outlive any one hub, so
 * a hermetic test that registers an owner must be able to start from empty. */
export function _resetTerminalViewOwners(): void {
  owners.clear();
  rows.clear();
  ownerSessions.clear();
}
