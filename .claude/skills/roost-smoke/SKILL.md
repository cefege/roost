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
- **This harness now costs live model calls and time.** Steps 15 and 16 each drive a real omp turn (step 16 retries once), so budget ~5 minutes worst case and 2–3 model turns per run. Everything up to step 14 is free and fast; there is no flag to skip the chat steps, because they are the only coverage the streaming and decision paths have.

## The flow — one-shot harness via `window.__smoke`

The harness script at `.claude/skills/roost-smoke/run.js` runs all 18 steps as a single `chrome_javascript` injection. It uses the SPA's smoke backdoor (`window.__smoke` from `apps/web/src/lib/smoke.ts`) instead of synthetic keyboard events, which means PTY input, kill, spawn, chat prompts, and state inspection all route through the SAME Connect-RPC client the real user does — JWT, auth, proto boundaries match production exactly.

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
4. **Interpret the return value:** the script resolves with `{ steps: [...], summary: "N/19 passed" }`. Each step has `{ name, pass, detail }`.

   Steps 15 and 16 each wait on a live model turn (up to 90s, and step 16 retries once), so the whole run can exceed 5 minutes. A single CDP `evaluate` will hit its protocol timeout: start the harness detached (`Promise.resolve(eval(src)).then(r => { window.__smokeResult = r; })`) and poll `window.__smokeResult` from short external calls. Also wait for `state().workers` to be non-empty AND `[data-testid="folder-list"]` to exist before injecting — injecting during the first sync makes every step fail with "no workers in store".

### Steps the harness covers

1. **bundle_loaded** — `<script src="/assets/index-*.js">` present, no `[data-testid="error-boundary"]`, `window.__smoke` installed.
2. **sidebar_bootstrap** — at least one `[data-testid^="machine-section-"]` + `[data-testid="sidebar-search"]`.
3. **workspace_spawn_and_nav** — spawn shell via `window.__smoke.spawnShell` then drive the cwd-picker submit; assert URL matches `/w/<id>/t/<channel>` within 8s and at least one `.wterm` is `visibility: visible`. Captures session_id for later steps.
4. _(folded into step 3 in the harness; named `step3_4_workspace_spawn_and_nav`)_
5. **echo_round_trip** — `window.__smoke.input(sessionId, "echo roostsmokehello\n")`, wait 800ms, assert `.term-grid` text contains `roostsmokehello`. NOTE: this is the coord-RPC path — it BYPASSES focus, so it cannot catch "can't input". step5b does.
5b. **input_focus_lands** — `window.__smoke.paneFocused(sessionId).focused === true`: the active pane's textarea IS `document.activeElement`, the only in-page proof a real keystroke would reach the PTY. Catches the cell-phase-3b cell-mode input bug (focus stuck on `<body>`).
6. **deck_persists_on_switch** — if 2+ session rows, click the other row, assert `[data-testid="terminal-deck"]` child count unchanged + exactly 1 `.wterm` visible. (Skipped vacuously if only one session — record passes with `detail: "skipped"`).
7. **kill_removes_row_and_cascades** — `window.__smoke.kill(sessionId)`, assert `state().sessions[id]` gone within 800ms. Workspace cascade-delete verified by comparing `workspace-row-*` counts.
8. **selected_state_url_driven** — `history.pushState` to a workspace URL, dispatch `popstate`, assert exactly one `[data-selected="focused"][data-testid^="workspace-row-"]` matches the URL.
9. **no_error_boundary_on_known_fail** — `window.__smoke.kill("00000000-...")` (non-existent session), assert no `[data-testid="error-boundary"]` appears.
13. **resize_wobble_holds_scrollback** — spawn shell, `seq 1 300` to fill scrollback, drive 3× sub-band (~4-row) deck-height wobbles (each waited > `FIT_SETTLE_MS`), assert `.term-scrollback-row` depth does not grow (`grew <= 2`). Hold-anchor hysteresis (`claimHysteresis.ts`) must absorb the wobble → no claim → no SCD change → no re-derive. Live regression case for the viewport-slaved-PTY history corruption.
14. **quick_chat_spawn_and_render** — `window.__smoke.quickChat()` then route to `/s/<sid>`; assert `[data-testid="omp-chat-pane"][data-session-id=<sid>]` AND its composer mount within 15s. Covers the whole one-tap path: candidate pick → `filesMkdir` → `spawnAgent` (which starts the `omp --mode rpc-ui` child as the session's OWN process, and fails the spawn RPC if it cannot) → sidebar bucket → deck routing. Eligibility is the session KIND (`session.kind === "agent"`), so no OSC title and no cwd heuristic is involved.
15. **chat_stream_round_trip** — send a real prompt through the Composer's RPC and sample the pane every 200ms. Pass needs **≥2 distinct growing assistant text lengths** (output really is incremental, not one lump at `message_end`) AND `sameNode`/`stillMounted` on the bubble element (the keyed `reconcile` in `chatOmp.ts` is holding, so `<For>` is not tearing the row down and rebuilding it every 60ms frame). Costs one model turn.
16. **chat_decision_multi_option** — ask the agent to use its `ask` tool, then assert an `[data-testid="omp-chat-approval"]` card with ≥3 `[data-testid="omp-chat-approval-option"]` controls and answer it. Proves omp is launched with a UI at all: `hasUI = isInteractive || mode === "rpc-ui"`, and without it `AskTool.createIf` returns null, so no decision can ever exist. The option COUNT (not just the card) also catches the worker's `options` mapping degrading to an empty array. Retries the prompt once; costs 1–2 model turns.
17. **terminal_and_chat_surfaces_are_disjoint** — THE independence requirement, in the DOM. Assert a `shell` session's slot holds `[data-testid="cell-terminal-pane"]` and NO `[data-testid="omp-chat-pane"]`, that emitting an omp OSC title (`π ⠹ working`) through its PTY does not change that (the `omp_eligible` latch is gone — a terminal running omp is still just a terminal), and conversely that step 14's `agent` session holds a chat pane and NO cell terminal. Replaces the old `chat_toggle_survives_run_state`: there is no toggle, because a session's kind picks its surface. No omp process, no model cost.
18. **chat_rows_all_painted** — read the rows the pane HOLDS (`window.__smoke.chatRows(sid).held`, the store projected through the same `roostMessageRows` the pane stamps from) and the rows it PAINTED (`[data-tui-row]` stamps filtered by `getClientRects().length > 0`). Pass needs `heldCount > 0`, `paintedCount === heldCount`, and equal `kind` order. **Painted, not stored** — a non-zero `heldCount` with a zero `paintedCount` means CSS (`.tr-row:not(:has(.tr-body > *))`) or the loading skeleton ate the rows, which no wire-level check can see. Both columns are read in the browser: the worker-side parity oracle is gone with the second engine it existed to arbitrate.

## Pass criteria

`summary` is `19/19 passed`. AND console has no errors matching:
- `Cannot read properties of null`
- `\[spawn-buttons\] (failed|timed out)`
- Toast text `did not (ack|respond)`

**Known-red as of 2026-07-25** (verified identical against stashed HEAD, so they
are pre-existing defects, NOT a licence to ignore a red): `step5b_input_focus_lands`
(`focused:false` with slot + textarea both present), `step6_deck_persists_on_switch`
(35→36 deck children on an account with ~33 sessions), `step13_resize_wobble_holds_scrollback`
(`d0:0` — scrollback rows never counted, in cell mode). Re-baseline before blaming
your change: stash, rebuild `@roost/web`, restart coord, re-run.

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
| step14 | quick-chat candidate/mkdir/spawn path (`apps/web/src/lib/quickChat.ts` → `spawnAgent`) — a failure here is the spawn or deck routing, not the chat engine. `spawnAgent` fails the RPC when omp cannot start, so a red here with a clear message usually means no omp binary on that worker |
| step15 | streaming: worker `STREAM_FLUSH_MS` coalescing (`rpc-chat.ts`) and the keyed `reconcile` in `chatOmp.ts`. `growthSamples < 2` = frames are not incremental; `sameNode:false`/`stillMounted:false` = `<For>` is remounting the row every frame |
| step16 | the omp child must be spawned `--mode rpc-ui` (`rpc-driver.ts`) or the `ask` tool does not exist; then `UI_DECISION_METHODS` in `rpc-chat.ts` must carry select/confirm/input/editor, and unrenderable methods must be DECLINED (a silent drop wedges the turn, since omp awaits every dialog request) |
| step17 | the two-subsystem split itself. `shellHasNoChat:false` = something re-coupled the chat pane to a terminal session (`TerminalDeck`'s kind branch, or `ompChatEnabled` reading anything other than `session.kind`). `agentPaintsChat:false` = an `agent` session is not routing to `OmpChatPane`. `stillNoChat:false` = an OSC-title eligibility latch is back |
| step18 | the DOM row stamps. `paintedCount: 0` with `heldCount > 0` = CSS or the skeleton is hiding rows (`chat-message.css` `.tr-row:not(:has(.tr-body > *))`, `OmpChatPane.tsx`'s `status !== "loading"` gate). `sameOrder: false` = `roostMessageRows`' block anchor no longer names the element that paints the row (`@roost/shared/chat/rows`, `OmpChatPane.tsx`'s `stamps` memo). `paintedCount` short of `heldCount` = rows the pane holds but never paints — the exact gap this step exists to name |

Do NOT patch the symptom. Open the linked memory, apply the named pattern.

## Worker-deploy variant

Same flow but after a fresh `bun apps/roost-cli/src/main.ts deploy <host>`. Catches the 4 stacked deploy bugs in [[feedback_worker_deploy_macos_repairs]].

## Output

End with one line: `roost-smoke: 19/19 passed` or `roost-smoke: STEP <K> FAILED: <reason> → see memory/<file>.md`.

No partial work. Do not declare a change done if the smoke didn't run end-to-end clean.
