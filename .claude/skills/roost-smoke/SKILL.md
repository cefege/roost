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

## The flow — shared browser harness

The injectable launcher at `.claude/skills/roost-smoke/run.js` delegates to
`window.__smoke.runFlow()`. The implementation is
`apps/web/src/lib/smokeHarness.ts`, shared with the isolated Playwright
scenario. Do not duplicate terminal checks in this launcher.

### Run procedure

1. Create a fresh tab on the current tailnet coord URL, never localhost.
2. Enable the smoke backdoor and reload:
   ```js
   localStorage.roostSmoke = "1"; location.reload();
   ```
3. Wait until `window.__smoke`, a worker in `state().workers`, and
   `[data-testid="folder-list"]` are present.
4. Inject `.claude/skills/roost-smoke/run.js` with `chrome_javascript`.
5. Every returned step must pass. `runFlow()` checks worker discovery, a
   shell’s painted terminal slot, RPC PTY marker round-trip, workspace
   creation, then cleanup of only harness-created resources.
6. The RPC marker deliberately bypasses the browser input pipeline. Before
   declaring the live canary green, create a separate smoke-tracked shell and
   route to it:
   ```js
   const fp = Object.keys(__smoke.state().workers)[0];
   const { session_id } = await __smoke.spawnShell(fp, "/tmp");
   history.pushState({}, "", `/s/${session_id}`);
   window.dispatchEvent(new PopStateEvent("popstate"));
   ```
   Wait until `[data-testid="terminal-slot-${session_id}"]` is visible, then
   click its terminal surface to establish focus before sending keys.
   Use `chrome_keyboard` to type `printf '%s\n' ROOST_LIVE_KEYBOARD_<nonce>`
   and Enter, then assert the rendered terminal text contains the exact
   marker. Finish with `await __smoke.cleanupCreated()`. This is the
   load-bearing focus/input proof; Playwright repeats it in CI.

The flow has a `finally` cleanup. A failed step is still a failure even if its
session and workspace were removed successfully.

## Pass criteria

All returned steps pass and the summary reports `N/N passed`. Console must not
contain `Cannot read properties of null`, a failed/timed-out spawn message, or
an acknowledgement timeout.

## On failure

The live run is a production canary. Preserve the returned failing step and
check the real terminal before editing. Do not patch the launcher: terminal
contracts live in `apps/web/src/lib/smokeHarness.ts` and
`smoke/terminal/terminal.spec.ts`.

## Output

End with `roost-smoke: <all steps passed>` or
`roost-smoke: STEP <name> FAILED: <reason>`.

No partial work. Do not declare a change done if the smoke didn't run end-to-end clean.
