// Worker-routability signal — the authoritative "is this worker usable" state.
// A2: routability = which workers the coord can reach over the raw WS RIGHT
// NOW (workersList.routableFps), NOT heartbeat freshness — a worker can keep
// heartbeating over the unary transport while its WS is down. Leaf module so
// the firehose (writes on the routable bus), bootstrap (writes on every
// workersList), and the UI (reads via workerOnline) share ONE signal without
// an import cycle.

import { createSignal } from "solid-js";
import type { Worker } from "@roost/shared/wire";

// null until the first list (then fall back to freshness so a pre-bootstrap
// render isn't all-offline). Custom equals: callers pass a FRESH Set per
// frame/refresh, so without content equality every workerRoutable frame and
// every workersList refetch invalidated ALL workerOnline() subscribers even
// when membership was unchanged. null compares unequal to any Set (first
// list must fire).
const [_routableFps, setRoutableFps] = createSignal<ReadonlySet<string> | null>(null, {
  equals: (a, b) => a !== null && b !== null && a.size === b.size && [...b].every((f) => a.has(f)),
});
export { setRoutableFps };

/** Authoritative "is this worker usable" — WS membership when known, else
 *  heartbeat freshness as a pre-bootstrap fallback. Reactive (reads the
 *  signal), so callers in JSX/memos re-run when routability changes. */
export function workerOnline(w: Worker): boolean {
  const r = _routableFps();
  if (r === null) return Date.now() - w.last_seen_ms < 90_000;
  return r.has(w.fp);
}
