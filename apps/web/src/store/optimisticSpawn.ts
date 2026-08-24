// Optimistic new-terminal spawn. The browser mints the session id and inserts
// a client-only shell placeholder so the shared pane deck can paint the tab
// immediately. The real spawn reuses that id and replaces the placeholder.

import { createSignal } from "solid-js";
import { asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { deleteStoreRecord, rootStore, setRootStore } from "./root.ts";
import { addToast } from "./toastStore.ts";
import { clampTerminalGeometry } from "@roost/shared/viewport";

// Reactive so CellTerminal's `pending` memo re-runs when a placeholder resolves.
// `aborted` needs no reactivity — it is read imperatively in doNewTab's resolve.
const [pendingIds, setPendingIds] = createSignal<ReadonlySet<string>>(new Set());
const aborted = new Set<string>();

export interface MountedSpawnMeasurement {
  cols: number;
  rows: number;
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

export function isPendingSpawn(id: string): boolean {
  return pendingIds().has(id);
}
export function wasAborted(id: string): boolean {
  return aborted.has(id);
}
export function clearAborted(id: string): void {
  aborted.delete(id);
}

/** Publish the mounted slot size exactly once as an initial PTY-size hint. */
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
    || !Number.isFinite(measurement.cols)
    || !Number.isFinite(measurement.rows)
    || measurement.cols <= 0
    || measurement.rows <= 0
  ) return false;
  deferred.done = true;
  deferred.resolve(clampTerminalGeometry(measurement));
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

// Spawn confirmation only releases the placeholder. The mounted terminal view
// attaches through its normal socket-bound command after the opened event.
export function endOptimisticSpawn(id: string): void {
  closeMeasurementDeferred(id);
  clearPending(id);
}

// Spawn failed: remove the placeholder (reconcile prunes the tab) + toast, unless
// the user already aborted (closed the pending tab) — then it's an expected removal.
export function failOptimisticSpawn(id: string, err: unknown): void {
  const wasAbortedNow = aborted.has(id);
  removePlaceholder(id);
  clearPending(id);
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
  if (rootStore.sessions[id]) deleteStoreRecord("sessions", id);
}
function clearPending(id: string): void {
  setPendingIds((s) => {
    if (!s.has(id)) return s;
    const n = new Set(s);
    n.delete(id);
    return n;
  });
}
