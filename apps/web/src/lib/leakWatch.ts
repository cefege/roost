// Always-on leak watchdog. A days-long browser session can't be reproduced
// synthetically, so instead the tab self-reports: a periodic accumulator sample
// plus a correlated snapshot on every long main-thread task. Feed the collected
// lines back to a deep-dive and the trajectory names what grows and when the
// UI starts stalling — the freeze-hunt evidence (per-session map sizes, DOM
// node count, heap) we otherwise only get by catching the aged tab live.
//
// Tier-1 signal() so it ships even with the diag firehose off; also console.info
// for easy copy from a live console. Emit-and-drop — the watcher retains nothing
// itself (no arrays, no growth), so it can never be its own leak.

import { diag, signal } from "@roost/shared/diag";
import { rootStore } from "../store/root.ts";
import { cellFrameCountSize } from "../store/sync-dispatch.ts";

// Accumulators to watch: anything keyed per-session (the leak class) + the
// gross DOM/heap totals. A climbing per-session count over a flat session count
// is a reaper miss; climbing dom_nodes/heap_mb with flat sessions is elsewhere.
function sample(): Record<string, number> {
  const mem = (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory;
  return {
    uptime_s: Math.round(performance.now() / 1000),
    heap_mb: mem ? Math.round(mem.usedJSHeapSize / 1e6) : -1,
    dom_nodes: document.getElementsByTagName("*").length,
    cell_grids: document.querySelectorAll(".cell-grid").length,
    cell_rows: document.querySelectorAll(".cell-row").length,
    held_sb_rows: document.querySelectorAll(".cell-scrollback .cell-row").length,
    sessions: Object.keys(rootStore.sessions).length,
    last_activity: Object.keys(rootStore.last_activity).length,
    session_viewers: Object.keys(rootStore.session_viewers).length,
    cell_frame_counts: cellFrameCountSize(),
  };
}

const SAMPLE_MS = 60_000;
const STALL_MS = 200;            // a task this long is a perceptible whole-UI freeze
const STALL_THROTTLE_MS = 10_000; // one Tier-1 stall signal per burst (also cooldown-gated)

/** Wire once from main.tsx (unconditional — the whole point is the natural,
 *  un-instrumented multi-day run). Best-effort: unsupported APIs no-op.
 *
 *  Periodic sample → diag() + console (verbose trajectory; gated firehose +
 *  always-visible in a live console for copy). A real freeze (main-thread task
 *  ≥ STALL_MS) → Tier-1 signal() carrying the accumulator snapshot AT stall
 *  time, so it ships to coord *.err.log the moment the tab stalls over days —
 *  the state at that instant names the growth (per-session map sizes vs
 *  dom_nodes vs heap). Kept rare (≥200ms + throttle + cooldown) to respect the
 *  Tier-1 daily-review channel. */
export function installLeakWatch(): void {
  if (typeof window === "undefined") return;
  // Manual inspection hook: run window.__leakSample() in any live console to
  // read the current accumulators on demand (no wait for the periodic tick).
  (window as Window & { __leakSample?: () => Record<string, number> }).__leakSample = sample;
  const periodic = (extra?: Record<string, number>): void => {
    const s = extra ? { ...sample(), ...extra } : sample();
    diag("diag.leak_sample", s);
    console.info("[leakwatch] sample", s);
  };
  try {
    periodic({ boot: 1 });
    setInterval(() => periodic(), SAMPLE_MS);
    let lastStall = -STALL_THROTTLE_MS; // so the first stall is never throttled
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) {
        if (e.duration < STALL_MS) continue;
        const now = performance.now();
        if (now - lastStall < STALL_THROTTLE_MS) return;
        lastStall = now;
        const s = { ...sample(), dur_ms: Math.round(e.duration), cooldownKey: 0 };
        signal("perf.longtask_stall", s);
        console.info("[leakwatch] STALL", s);
        return;
      }
    }).observe({ type: "longtask", buffered: false });
  } catch {
    /* PerformanceObserver('longtask') / performance.memory unsupported — the
       periodic sample above still installs; the correlation is best-effort. */
  }
}
