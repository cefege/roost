// Optimistic new-terminal spawn. The browser mints the session id and inserts
// a client-only shell placeholder so the shared pane deck can paint the tab
// immediately. The real spawn reuses that id and replaces the placeholder.

import { createSignal } from "solid-js";
import { asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "./root.ts";
import { addToast } from "./toastStore.ts";

// Reactive so CellTerminal's `pending` memo re-runs when a placeholder resolves.
// `aborted` needs no reactivity — it is read imperatively in doNewTab's resolve.
const [pendingIds, setPendingIds] = createSignal<ReadonlySet<string>>(new Set());
const aborted = new Set<string>();

export interface MountedSpawnMeasurement {
  cols: number;
  rows: number;
  clientSeq: number;
}

export interface SpawnViewportSeed extends MountedSpawnMeasurement {
  effectiveClientSeq: number;
}

interface MeasurementDeferred {
  promise: Promise<MountedSpawnMeasurement | null>;
  resolve: (value: MountedSpawnMeasurement | null) => void;
  done: boolean;
  closed: boolean;
}

// One deferred per optimistic placeholder. It is created before the placeholder
// enters the Solid store, so a synchronously mounted CellTerminal can resolve it
// without racing doNewTab's waiter.
const measurementDeferreds = new Map<string, MeasurementDeferred>();
// Spawn response state is consumed once by the already-mounted pane. Seeding
// before clearing pending makes the pending→false INITIAL effect deterministic.
const viewportSeeds = new Map<string, SpawnViewportSeed>();

export function isPendingSpawn(id: string): boolean {
  return pendingIds().has(id);
}
export function wasAborted(id: string): boolean {
  return aborted.has(id);
}
export function clearAborted(id: string): void {
  aborted.delete(id);
}

/** Publish the real mounted slot size exactly once, after renderer + cell
 * handler installation. Late measurements after the 100ms caller deadline are
 * rejected so an ordinary fallback spawn can never be mislabeled preclaimed. */
export function publishMountedSpawnMeasurement(
  id: string,
  measurement: MountedSpawnMeasurement,
): boolean {
  const deferred = measurementDeferreds.get(id);
  if (
    !deferred
    || deferred.done
    || deferred.closed
    || !pendingIds().has(id)
    || !Number.isInteger(measurement.cols)
    || !Number.isInteger(measurement.rows)
    || !Number.isSafeInteger(measurement.clientSeq)
    || measurement.cols <= 0
    || measurement.rows <= 0
    || measurement.clientSeq <= 0
  ) return false;
  deferred.done = true;
  deferred.resolve(measurement);
  return true;
}

/** Wait at most `timeoutMs` for the mounted placeholder. null means use the
 * ordinary estimate-only spawn path; the deferred is closed on every exit. */
export async function waitForMountedSpawnMeasurement(
  id: string,
  timeoutMs = 100,
): Promise<MountedSpawnMeasurement | null> {
  const deferred = measurementDeferreds.get(id);
  if (!deferred || deferred.closed) return null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      deferred.promise,
      new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), Math.max(0, timeoutMs));
      }),
    ]);
  } finally {
    clearTimeout(timer);
    deferred.closed = true;
    if (measurementDeferreds.get(id) === deferred) measurementDeferreds.delete(id);
  }
}

/** Consume the coordinator-owned preclaim watermark once. */
export function takeSpawnViewportSeed(id: string): SpawnViewportSeed | null {
  const seed = viewportSeeds.get(id) ?? null;
  viewportSeeds.delete(id);
  return seed;
}

// Insert a shell placeholder in the bucket selected by the anchor tab.
export function beginOptimisticSpawn(anchor: Session): string {
  const id = crypto.randomUUID();
  const { promise, resolve } = Promise.withResolvers<MountedSpawnMeasurement | null>();
  measurementDeferreds.set(id, { promise, resolve, done: false, closed: false });
  const folder = anchor.cwd;
  const placeholder: Session = {
    id: asSessionId(id),
    worker_fp: anchor.worker_fp,
    channel: asChannelId(0), // sentinel; real channel arrives on `opened`
    kind: "shell",
    cwd: folder,
    spawn_cwd: folder,
    workspace_id: anchor.workspace_id ?? null,
    status: "open",
    created_at: Date.now(), // liveIds sorts by this → appends last
    closed_at: null,
    custom_title: null,
  };
  setRootStore("sessions", id, placeholder);
  setPendingIds((s) => {
    const n = new Set(s);
    n.add(id);
    return n;
  });
  return id;
}

// Spawn confirmed. When the measured viewport was preclaimed, publish its
// coordinator-owned watermark before dropping pending; the mounted terminal
// consumes it synchronously and suppresses only the equivalent INITIAL claim.
export function endOptimisticSpawn(id: string, seed?: SpawnViewportSeed): void {
  if (seed) viewportSeeds.set(id, seed);
  closeMeasurementDeferred(id);
  clearPending(id);
}

// Spawn failed: remove the placeholder (reconcile prunes the tab) + toast, unless
// the user already aborted (closed the pending tab) — then it's an expected removal.
export function failOptimisticSpawn(id: string, err: unknown): void {
  const wasAbortedNow = aborted.has(id);
  removePlaceholder(id);
  clearPending(id);
  viewportSeeds.delete(id);
  closeMeasurementDeferred(id);
  aborted.delete(id);
  if (!wasAbortedNow) {
    addToast(`New terminal failed: ${err instanceof Error ? err.message : String(err)}`, "err");
  }
}

// User closed the placeholder before the spawn resolved: remove it now; the
// caller reaps the real PTY once the in-flight spawn lands (see doClose + doNewTab).
export function abortOptimisticSpawn(id: string): void {
  aborted.add(id);
  removePlaceholder(id);
  viewportSeeds.delete(id);
  closeMeasurementDeferred(id);
  clearPending(id);
}

function closeMeasurementDeferred(id: string): void {
  const deferred = measurementDeferreds.get(id);
  if (!deferred) return;
  deferred.closed = true;
  measurementDeferreds.delete(id);
  if (!deferred.done) {
    deferred.done = true;
    deferred.resolve(null);
  }
}

function removePlaceholder(id: string): void {
  if (rootStore.sessions[id]) setRootStore("sessions", id, undefined as unknown as Session);
}
function clearPending(id: string): void {
  setPendingIds((s) => {
    if (!s.has(id)) return s;
    const n = new Set(s);
    n.delete(id);
    return n;
  });
}
