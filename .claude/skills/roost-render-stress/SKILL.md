---
name: roost-render-stress
description: Loop-and-log render-correctness harness — hammers terminal viewport resizes (width/height/both, in/out of band) + tab-switch + multi-viewer in a loop and asserts the painted grid never duplicates, loses, or mis-orders history. Catches the corruption class wire/data-* smoke is blind to. Run in BOTH byte and cell mode before claiming any terminal change works.
---

# Roost render-correctness stress harness

The wire smoke (`roost-smoke`) asserts data flow + data-* flips. It is **blind**
to what actually paints. Every recurring terminal nightmare lives in the paint:
scroll jumps to top on refresh, rows duplicate / mis-position on tab-switch,
history shrinks after resize. This harness asserts the PAINT, under the exact
perturbation that drives the corruption (viewport-size wobble →
`project_terminal_history_corruption_viewport_slaved_pty`).

Hard rule (Author 2026-06-22): no terminal-render development is "done" until
this harness runs clean in BOTH modes. Green wire tests are not evidence.

THE SUBJECT MUST BE A REAL OMP-BACKED SESSION. Start a shell, launch `omp`,
and wait until its bridge transcript is connected before driving it. A synthetic
shell-only marker stream exercises main-screen scrollback but not OMP lifecycle
or the structured-state projection. It may supplement, never replace, the
OMP session pass.

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

## Run procedure (per mode)

1. Fresh tab on the tailnet URL. Enable backdoor: `localStorage.roostSmoke="1"`.
   For cell mode also `localStorage.roostCellMode="1"`. Then `location.reload()`.
2. Spawn a shell on the target worker, launch OMP, and wait for its bridge
   transcript before producing a unique marker table:
   ```js
   const fp = Object.values(__smoke.state().workers)
     .find(w => w.label === "<target>").fp;
   const { session_id } = await __smoke.spawnShell(fp, "/tmp");
   history.pushState({}, "", "/s/" + session_id);
   window.dispatchEvent(new PopStateEvent("popstate"));
   await __smoke.input(session_id, "omp\r");
   // Wait for rootStore.omp_transcript[session_id], then request 60 rows whose
   // IDs are CELLLINE-1 through CELLLINE-60.
   ```
   Poll `__smoke.markerScan(session_id, "CELLLINE-")` until the marker range
   settles.
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
   while the run executes. Run the same loop in byte and cell mode, then run
   the multi-viewer procedure below.

## Multi-viewer / SCD — the two-device mangle (Author 2026-06-22)

Observed live: on the PRIORITY device (the SCD-min one) the terminal + back
history render clean; on a SECONDARY, LARGER co-viewer the same session is
"completely mangled". Cause: PTY is sized to SCD-min (smallest viewer); in byte
mode the stream is wrapped at that smaller width, and the larger viewer
re-wraps already-wrapped lines → rows out of position. `markerScan.outOfOrder`
is the detector. (Cells letterbox at the worker width and never re-wrap → this
is the case cell mode must win; verify it does once cell's tab-switch bug is fixed.)

5. Open two tabs for the same OMP-backed session at different deck sizes. Make
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

One line per mode: `render-stress[byte]: PASS (80 iters)` or
`render-stress[cell]: FAIL — 14/80 iters, first DUP at op=h-shrink i=3`.
Do NOT declare a terminal change done until BOTH modes + the multi-viewer case
are PASS.
