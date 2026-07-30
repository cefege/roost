---
name: roost-render-stress
description: Loop-and-log render-correctness harness — hammers terminal viewport resizes (width/height/both, in/out of band) + tab-switch + multi-viewer in a loop and asserts the painted grid never duplicates, loses, or mis-orders history. Catches the corruption class wire/data-* smoke is blind to.
---

# Roost render-correctness stress harness

The wire smoke (`roost-smoke`) asserts data flow + data-* flips. It is **blind**
to what actually paints. Every recurring terminal nightmare lives in the paint:
scroll jumps to top on refresh, rows duplicate / mis-position on tab-switch,
history shrinks after resize. This harness asserts the PAINT, under the exact
perturbation that drives the corruption (viewport-size wobble →
`project_terminal_history_corruption_viewport_slaved_pty`).

Hard rule (Author 2026-06-22): no terminal-render development is "done" until
this harness and the multi-viewer case run clean. Green wire tests are not evidence.

THE SUBJECT MUST BE A REAL INTERACTIVE PTY SESSION. Start a shell and wait for
its prompt before driving it. An agent CLI such as `omp` or Claude Code, or a
TUI such as `vim`, may run inside that PTY when it is the subject under test,
but Roost observes only the terminal grid. Synthetic DOM or store injection may
supplement, never replace, the real PTY pass.

FOREGROUND GOTCHA: a backgrounded tab throttles setTimeout/rAF → the loop
stalls AND the cell renderer stops painting → readings are stale (false PASS),
per `feedback_humanchrome_render_test_false_positives`. `chrome_switch_tab` to
the tab under test BEFORE each run and assert `document.hidden===false`. For
multi-viewer, foreground each viewer in turn to force its repaint, then probe.

VISIBILITY PIN (`__smoke.forceVisible(true)`, lib/pageVisible.ts): pins
app-level visibility to foreground — a backgrounded tab keeps its viewport
claims and sync stream instead of withdrawing (the depth-frozen-at-0 false
FAIL). Use it when the tab under test can lose foreground (user activity in
the same Chrome). It does NOT defeat browser-level setTimeout/rAF throttling:
in-page polling loops still stall on hidden tabs (drive the loop with SHORT
external chrome_javascript calls instead), and Chrome may starve a
long-backgrounded tab's HTTP/2 stream at the transport layer — keep hidden
windows short. Turn it OFF (`forceVisible(false)`) before leaving the tab.
Cell frames paint synchronously in the WS handler (cellRenderer.apply), so
arrived frames are probe-readable even while hidden. Verified 2026-07-11:
hidden tab, pin on → 264 scrollback rows painted with document.hidden===true.

## Invariants asserted, every iteration

Over every rendered row, marker IDs must be unique and monotonic
(`CELLLINE-<n>`):
- **No duplication** — `markerScan.duplicated == []`.
- **No mangle** — `markerScan.outOfOrder == 0`.
- **Depth** — alternate-screen views may legitimately expose a different marker
  range after a resize; only duplicates and order are invariants there.

## Prerequisites

- Live coord on the tailnet URL; never localhost.
- A worker with a healthy keeper. A degraded keeper produces dead PTYs and
  stalled input; that is a product finding, not a harness failure.

## Run procedure

1. Fresh tab on the tailnet URL. Enable the backdoor with
   `localStorage.roostSmoke="1"`, then reload.
2. Spawn a shell on the target worker, wait for its prompt, and produce a
   unique marker table through PTY input:
   ```js
   const fp = Object.values(__smoke.state().workers)
     .find(w => w.label === "<target>").fp;
   const { session_id } = await __smoke.spawnShell(fp, "/tmp");
   history.pushState({}, "", "/s/" + session_id);
   window.dispatchEvent(new PopStateEvent("popstate"));
   await __smoke.input(
     session_id,
     "i=1; while [ $i -le 60 ]; do printf 'CELLLINE-%s\\n' \"$i\"; i=$((i+1)); done\r",
   );
   ```
   Poll `__smoke.markerScan(session_id, "CELLLINE-")` until the marker range
   settles. This readiness gate is painted terminal output, not agent state.
3. Set the runner inputs and inject `run.js`:
   ```js
   window.__stressSid = session_id;
   window.__stressPrefix = "CELLLINE-";
   window.__stressScreen = "alt";
   window.__stressIter = 80;
   ```
   The launcher delegates to `__smoke.runRenderStress()`, which perturbs the
   real terminal deck and probes every painted frame. For `alt`, only duplicate
   or out-of-order markers fail; for `main`, a changed marker range also fails.
4. Read `{ verdict, iterations, failCount, fails }`. Keep the tab foreground
   while the run executes, then run the multi-viewer procedure below.

## Multi-viewer / SCD — the two-device mangle (Author 2026-06-22)

Observed live: the terminal and history rendered clean on the smaller priority
viewer but were mangled on a larger co-viewer. Roost now has one canonical
cell-grid renderer, which letterboxes at the worker width rather than reflowing
history. `markerScan.outOfOrder` remains the detector for any regression.

5. Open two tabs for the same interactive PTY session at different deck sizes. Make
   tab A small and tab B noticeably larger. Foreground each in turn, settle,
   then probe `__smoke.markerScan(id, "CELLLINE-")`:
   - PASS requires both tabs to have `duplicated == []` and `outOfOrder == 0`.
     Different viewport heights may expose different marker ranges.
6. Resize-hammer tab A while tab B sits passive → foreground B and probe it (a
   resizer must never mangle a passive co-viewer). Then the symmetric direction:
   resize-hammer B while A passive → foreground A and probe. Background tabs
   don't repaint, so you MUST foreground the passive viewer to read it truthfully.

## Cleanup (NEVER skip, NEVER mass-kill)

`await __smoke.cleanupCreated()` kills and deletes only resources created by
this smoke tab. NEVER iterate `state().sessions` to kill live sessions.

## Output

Report `render-stress: PASS (80 iters)` or
`render-stress: FAIL — 14/80 iters, first DUP at op=h-shrink i=3`.
Do not declare a terminal change done until the stress loop and multi-viewer
case both pass.
