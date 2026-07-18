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

THE SUBJECT MUST BE A REAL CLAUDE SESSION (Author 2026-06-22b): a plain shell
echoing `ROOSTLINE-$i` exercises MAIN-screen scrollback — a path that was never
broken. It passes GREEN while the actual bug is untouched = NOT a test. The
corruption (the mobile screenshot: a claude table line stamped ~40× with tail
mangle) lives in the **claude alt-screen redraw** on resize / multi-viewer SCD
rebuild (`apps/worker/src/session-manager.ts::_rebuildWtermCore`, fix c03d62d0).
Drive `__smoke.spawnClaude` and render rich content (a numbered table). Shell is
at most a supplementary scrollback-depth check, NEVER the primary proof.

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

Over EVERY rendered row (probes in `apps/web/src/lib/smoke.ts`), markers must be
UNIQUE + monotonic (`CELLLINE-<n>`; claude PROSE repeats → false dup):
- **No duplication** — `markerScan.duplicated == []` (a marker N painted twice = the stamp-40× mangle).
- **No mangle** — `markerScan.outOfOrder == 0` (rows out of position = re-wrap / bad re-derive). LOAD-BEARING for claude + multi-viewer.
- **Depth** — claude alt-screen has NO scrollback and REFLOWS its TUI per width, so `max` legitimately varies (only the viewport paints). Do NOT require `max==baseline` for claude. (Shell-scrollback supplementary check still asserts depth retained + recovery.)

## Prerequisites

- Live coord on the tailnet URL (resolve host via `tailscale status`; never localhost).
- A worker with a HEALTHY keeper. A degraded long-lived keeper births dead PTYs
  (new shells produce no output, `sessionsInput` hangs) — see
  `feedback_claude_code_runs_inside_roost_keeper_pty`. Pick the worker whose
  `__smoke.state().workers[].last_seen_ms` is freshest; if spawn/input hangs,
  the keeper is degraded — that is a finding, not a harness bug.

## Run procedure (per mode)

1. Fresh tab on the tailnet URL. Enable backdoor: `localStorage.roostSmoke="1"`.
   For cell mode also `localStorage.roostCellMode="1"`. Then `location.reload()`.
2. Spawn a REAL CLAUDE session (NOT a shell) on the target worker and have it
   render a table with unique monotonic markers:
   ```js
   const fp = Object.values(__smoke.state().workers)
     .find(w => w.label === "<target>").fp;   // explicit worker, e.g. worker-host
   const { session_id } = await __smoke.spawnClaude(fp, "/tmp");
   history.pushState({}, "", "/s/"+session_id);
   window.dispatchEvent(new PopStateEvent("popstate"));
   // wait for the "❯" input prompt + "Remote Control active", then:
   await __smoke.input(session_id,
     "Print ONLY a markdown table, no preamble. Columns ID and WORD. Exactly 60 "+
     "rows; row i = | CELLLINE-i | word-i | for i 1..60. Output nothing else.");
   await __smoke.input(session_id, "\r");
   ```
   Poll `__smoke.markerScan(session_id,"CELLLINE-")` until `max` stops growing
   (claude streamed the table). Only the viewport tail paints (alt-screen) — a
   small `unique` count is correct, the stamp-40× mangle still shows as `dup`.
3. Run the detached resize loop (perturbation matrix in `run.js`:
   w-shrink/grow, h-shrink/grow, both, plus tiny in-band wobble; half the iters
   wobble back to base). Drive `deck.style.width/height/maxHeight` over ~70-80
   iters at ~400ms settle, calling `markerScan(sid,"CELLLINE-")` after each.
   `run.js` is the SHELL variant (asserts depth/loss/recovery — valid only for a
   shell-scrollback supplementary run). For claude, fail ONLY on
   `duplicated.length || outOfOrder` — see step 4.
4. Read the verdict: `{ verdict: "PASS"|"FAIL", failCount, fails:[...] }`.
   For claude the fail condition is `duplicated.length>0 || outOfOrder>0` ONLY
   (`DUP=`/`MANGLE`); do NOT fail on depth change. Each fail row names the op
   (`w-shrink`/`h-grow`/`both-shrink`/…). The loop runs ~1-2 iters/s and exceeds
   the 16s CDP cap — launch it DETACHED (write result to `window.__cR`, set a
   `window.__cP` progress counter, wrap in try/finally so a throw still clears
   the running flag) and poll `window.__cR` with short separate calls. Keep the
   tab foreground the whole run.

## Multi-viewer / SCD — the two-device mangle (Author 2026-06-22)

Observed live: on the PRIORITY device (the SCD-min one) the terminal + back
history render clean; on a SECONDARY, LARGER co-viewer the same session is
"completely mangled". Cause: PTY is sized to SCD-min (smallest viewer); in byte
mode the stream is wrapped at that smaller width, and the larger viewer
re-wraps already-wrapped lines → rows out of position. `markerScan.outOfOrder`
is the detector. (Cells letterbox at the worker width and never re-wrap → this
is the case cell mode must win; verify it does once cell's tab-switch bug is fixed.)

5. Two tabs on the same claude `/s/<id>` at DIFFERENT deck sizes (set
   `deck.style.width/height` inline; same window can't show two sizes otherwise)
   — tab A small (the SCD-min driver), tab B noticeably LARGER (the secondary).
   Foreground each in turn, settle, probe `__smoke.markerScan(id,"CELLLINE-")`:
   - PASS requires BOTH tabs: `duplicated==[]` and **`outOfOrder==0`** (the
     larger tab is the one that historically mangles — load-bearing). `max` will
     differ between tabs (different viewport heights) — that is NOT a failure.
6. Resize-hammer tab A while tab B sits passive → foreground B and probe it (a
   resizer must never mangle a passive co-viewer). Then the symmetric direction:
   resize-hammer B while A passive → foreground A and probe. Background tabs
   don't repaint, so you MUST foreground the passive viewer to read it truthfully.

## Cleanup (NEVER skip, NEVER mass-kill)

`await __smoke.killSpawned()` — kills ONLY this tab's spawns. NEVER iterate
`state().sessions` to kill (`feedback_never_mass_kill_live_sessions`).

## Output

One line per mode: `render-stress[byte]: PASS (80 iters)` or
`render-stress[cell]: FAIL — 14/80 iters, first DUP at op=h-shrink i=3`.
Do NOT declare a terminal change done until BOTH modes + the multi-viewer case
are PASS.
