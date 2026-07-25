---
name: roost-smoke
description: Drive the full Roost end-to-end user flow via humanchrome — workspace create, terminal opens, echo round-trip, pane close, workspace cascade-delete. Catches every recurring failure mode from CLAUDE.md L11.
---

# Roost smoke test (humanchrome, not Playwright)

This skill is the integration-test floor. Run it after **every** non-trivial change to `apps/web/`, `apps/worker/`, `apps/coord/`, or `apps/shared/` before claiming done. Per [[feedback_playwright_only_no_humanchrome]] this uses humanchrome, never Playwright.

## When to invoke

- After any change to `MainPane.tsx`, `Terminal.tsx`, `SessionRow.tsx`, `WorkspaceRow.tsx`, `MachineSection.tsx`, `event-log.ts`, `workspaces.ts` (coord), or any wire shape.
- After a worker deploy (`bun apps/roost-cli/src/main.ts deploy <host>`).
- Pre-push verification (manual; pre-push hook only runs `lint-roost.ts`).
- Any user-reported regression that matches a CLAUDE.md L11 row.

## API-first scope — when the browser pass is NOT required

`smoke/api_smoke.test.ts` (headless, pure Connect RPC, <10s) covers the
DATA-PLANE assertions of steps 3, 5, and 7: spawn-reaches-open, echo
round-trip (SessionsInput → GetScrollbackSince marker), rename set/clear,
workspace lifecycle incl. assign + delete-membership cascade, Sync-stream
session_event delivery, and kill-reaches-closed. Run it with:

```sh
ROOST_API_SMOKE=1 ROOST_COORD_URL=https://<coord-host>:4102 bun test smoke/api_smoke.test.ts
```

For changes that touch ONLY the data plane (coord handlers, worker session
paths, proto wire shapes — nothing under `apps/web/`), the API harness is a
sufficient smoke; skip the browser pass. The full humanchrome flow below
remains REQUIRED for anything touching the SPA: paint correctness, focus
(step 5b), deck persistence (6), selected-state (8), error boundaries (9),
resize wobble (13), and sidebar DOM are irreducibly browser-side (L11 G8) —
the API harness is blind to them. The run.js steps themselves stay: the
browser flow still exercises the SPA side of the same mutations.

## Prerequisites

- coord-v2 LaunchAgent running on `:4102` (M1)
- worker-v2 LaunchAgent running on `:2224` (M1 minimum; M5 too if testing multi-worker)
- The Chrome instance running humanchrome's MCP extension is open AND authorized against coord (browser pubkey in `authorized_keys`). If 401s appear, run [[reference_live_coord_pubkey_bootstrap]] before the smoke.

## The flow — one-shot harness via `window.__smoke`

The harness script at `.claude/skills/roost-smoke/run.js` runs its terminal flow as a single `chrome_javascript` injection. It uses the SPA's smoke backdoor (`window.__smoke` from `apps/web/src/lib/smoke.ts`) instead of synthetic keyboard events, which means PTY input, kill, spawn, and state inspection all route through the SAME Connect-RPC client the real user does — JWT, auth, proto boundaries match production exactly.

### Run procedure

1. **Create a fresh tab** on the tailnet URL (`https://coord-host.tailXXXXXX.ts.net:4102/` as of 2026-06-19; was server-a before the 2026-06-17 rename — resolve the live coord host via `tailscale status`), never localhost. NOTE: a fresh browser/IndexedDB 403s on the loopback-only `AuthAuthorizeBrowser` over the tailnet → authorize its pubkey via loopback on the coord host first (see [[live-coord-pubkey-bootstrap]]).
2. **Enable the backdoor + reload:**
   ```js
   localStorage.roostSmoke = "1"; location.reload();
   ```
   After reload, `window.__smoke` is installed (console: `[smoke] backdoor installed via window.__smoke`).
2b. **Fresh-profile hazards (both cause false FAILs):** (a) the What's New
   modal autofocuses + inerts the page on any profile whose
   `localStorage["roost.whatsNew.lastSeenVersion"]` ≠ current VITE_APP_VERSION —
   dismiss it (click "Got it") before spawning, or step5b's focus assert dies
   against the inert background. (b) If the tab can lose foreground mid-run
   (user activity in the same Chrome), call `window.__smoke.forceVisible(true)`
   after reload — pins app-level visibility so hidden tabs keep claims + sync
   stream (lib/pageVisible.ts; browser-level setTimeout/rAF throttle still
   applies, so prefer short external calls over long in-page polls). Unpin
   when done.
3. **Inject the harness** in one `chrome_javascript` call. Read `.claude/skills/roost-smoke/run.js` and pass it as the `code` argument (wrap with `return await (...)` if needed for the IIFE).
4. **Interpret the return value:** the script resolves with `{ steps: [...], summary: "N/18 passed" }`. Each step has `{ name, pass, detail }`. Also wait for `state().workers` to be non-empty AND `[data-testid="folder-list"]` to exist before injecting — injecting during the first sync makes every step fail with "no workers in store".

### Steps the harness covers

1. **bundle_loaded** — `<script src="/assets/index-*.js">` present, no `[data-testid="error-boundary"]`, `window.__smoke` installed.
2. **sidebar_bootstrap** — folder list and search are mounted.
2b. **fresh_deck_ready** — dismisses What’s New when present, waits for it to unmount, then routes to `/` and waits for `[data-testid="home-landing"]`.
3. **spawn_and_nav** — spawns a shell and navigates to its flat session route. **mount_footprint** records open sessions, mounted slots, cell panes, `.wterm` roots, textareas, visible slots, and mounted/visible session-ID sets; it requires `mountedIds == visibleIds`.
4–5b. **folder grouping, echo, and input focus** — confirm sidebar grouping, RPC echo, and that the active pane’s real textarea is `document.activeElement`.
6. **deck persistence** — on a folder switch, original slot and `CellTerminal` nodes remain connected but hidden, one slot is focused, mounted IDs equal the prior warm set union current visible IDs, and navigation back restores the original nodes and focus.
7–12. **close, error, workspace, pane, cwd, and attachment checks** — retain their existing RPC/store contracts.
13. **resize_wobble_holds_scrollback** — spawn shell, `seq 1 300` to fill scrollback, drive 3× sub-band (~4-row) deck-height wobbles (each waited > `FIT_SETTLE_MS`), and count `.cell-scrollback .cell-row` in the visible slot; depth must begin above 50 and grow by no more than 2. Hold-anchor hysteresis (`claimHysteresis.ts`) must absorb the wobble → no claim → no SCD change → no re-derive.

## Pass criteria

`summary` is `18/18 passed`. AND console has no errors matching:
- `Cannot read properties of null`
- `\[spawn-buttons\] (failed|timed out)`
- Toast text `did not (ack|respond)`

**Known-red as of 2026-07-25** (verified identical against stashed HEAD, so it
is pre-existing, not a licence to ignore a red):
`step13_resize_wobble_holds_scrollback` (`d0:0` — scrollback rows never
counted in cell mode). Re-baseline before blaming your change: stash, rebuild
`@roost/web`, restart coord, re-run.

## On failure

Each step name maps to a CLAUDE.md L11 row:

| step | L11 row |
|---|---|
| step3_4 | `feedback_worker_deploy_macos_repairs.md` (+ New workspace silent hang) |
| step5 | `feedback_no_props_read_in_oncleanup.md` (Terminal mount/unmount errors break input) |
| step5b | CellTerminal focus effect must be non-deferred (cell-phase-3b "can't input in cell mode"; focus stuck on `<body>`) |
| step6 | `feedback_persistent_terminal_deck.md` (Terminal remount on nav) |
| step7 | `feedback_worker_ack_required_for_kill.md` (✕ button does nothing) |
| step7 | `feedback_solid_setstore_record_replace.md` (store doesn't reflect delete) |
| step8 | `feedback_selected_means_url_match_not_has_children.md` |
| step13 | `project_terminal_history_corruption_viewport_slaved_pty.md` (history grows/corrupts on resize wobble) |

Do NOT patch the symptom. Open the linked memory, apply the named pattern.

## Worker-deploy variant

Same flow but after a fresh `bun apps/roost-cli/src/main.ts deploy <host>`. Catches the 4 stacked deploy bugs in [[feedback_worker_deploy_macos_repairs]].

## Output

End with one line: `roost-smoke: 18/18 passed` or `roost-smoke: STEP <K> FAILED: <reason> → see memory/<file>.md`.

No partial work. Do not declare a change done if the smoke didn't run end-to-end clean.
