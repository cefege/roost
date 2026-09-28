# Coord lead handoff — Stage 2C, wave 3 committed

**A moment, not a state.** Read `git status --porcelain` and `git log --oneline -20`
in the coord worktree before trusting any line below. The plan
(`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2C") wins.

## Build environment (host `/home/mike/repos`)

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 \
       CARGO_TARGET_DIR=<worktree>/target-track
flock <worktree>/target-track/.roost-build.lock /home/mike/repos/roost-build-slot cargo …
```

Never end a turn to wait (API 400 crash); block in the foreground. Path-restricted
`git add` only. Commits need `GIT_AUTHOR_*`/`GIT_COMMITTER_*` env (no git identity is
configured on this host). JS: `bun install`; `apps/web/dist` must be a SMOKE build
(`VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`, untracked). One spec against the
Rust coord: `ROOST_SMOKE_COORD_EXECUTABLE=$PWD/target-track/debug/roost
ROOST_TEST_BUN=$(command -v bun) bun node_modules/@playwright/test/cli.js test
--config=playwright.config.ts --project=chromium-desktop --workers=1 smoke/terminal/<spec>`
(`@serial` specs such as terminal-local-fast-path need `--project=chromium-serial`).
After every track gate run `/home/mike/repos/roost-target-sweep <target-track>` under the
build lock.

## Committed on v3-coord (this session)

| SHA | What |
|---|---|
| `14f076df` | cherry-pick of worker `5208e0f9`: inactive agent status decodes as retirement |
| `0e2afee6` | RenderHistory: `pump_delta` names the cursor stream; terminal-render 3/5 → 4/5 |
| `7b19778b` | cherry-pick of worker `7a46b99a`: terminal_capture protocol types |
| `09f1b60c` | protocol: `fleet_update` classifier moved from roost-cli to roost-protocol (self-contained; integrator cherry-pick candidate) |
| `11b96998` | C-RETAIN |
| `77656f38` | C-DIRECT (2 rows) + revoked-generation fence fix (`frame_dispatch.rs`) |
| `ad0602e2` | AT (3 rows) |
| `b90351f8` | GS (2 rows) |
| `90a293a1` | D1 (2 rows) + streaming auth gate + arm-parser test fix |

The wave-3 series compiles as a whole at `90a293a1` (checked: `cargo check -p roost-coord
-p roost-cli --all-targets` on a clean checkout, 0 errors 0 warnings).

**Committed ratchet at `90a293a1`: AwaitingDomainPort 2 (`SessionsPrompt`, `DiagSnapshot`).**

## Spec evidence (Rust coord debug build of the combined wave-3 tree, TS worker, smoke dist)

- terminal-render: 4/5. Remaining: "streaming sequence repair leaves an off-bottom reader
  fixed" → `__smoke.terminalStreamProbe` → DiagSnapshot (X2).
- attachment-direct 3/3, global-search 1/1.
- terminal-peer 3/6: `:61` fails `goto: Download is starting` on the worker's loopback
  door — fails identically with the TS coordinator (environment, not coord); `:96` needs
  DiagSnapshot (X2); `:209` passes with the TS coord and FAILS with the Rust coord at the
  SECOND `waitForSyncRoute` (after `__smoke.pauseSyncTransport()`/`resumeSyncTransport()`
  the Sync route never re-reaches baselineReady && syncReady) — an open Sync-resume
  defect, not yet diagnosed.
- terminal-local-fast-path: its only test is `@serial`; not yet run (chromium-serial).

## In flight (helpers, uncommitted in the worktree)

- AG2 (`agents/{prompt_control,rpc_prompt}.rs`, `workers/terminal_send.rs` agent-prompt
  sender, `terminal_input/write_control.rs` WorkerWritten acceptance, tests
  `agent_prompt_*`): done; arm + row applied in the worktree, not committed.
- X2 DiagSnapshot (`diagnostics/{diag_snapshot,worker_results,session_state}.rs`,
  `workers/diag_send.rs`, tests `diag_snapshot_*`).
- C-PUSH (fold deletes on retirement; production Web Push transport; viewer suppression;
  `serve.rs` install; tests `push_*`, `agent_status_retirement`).
- C-CAPTURE (new `terminal_capture/`, reusing `roost_protocol::terminal_capture`).

## Next steps, in order

1. Land AG2, X2, C-PUSH, C-CAPTURE (arms/rows applied by the lead), each gated.
2. terminal-render 5/5 and terminal-peer `:96` after X2; diagnose terminal-peer `:209`.
3. Header audit (v2 basename in a `//!` line or README) for every `apps/coord/src` module.
4. Track gate (two agreeing test runs, clippy, lint, fmt) → release live-stack READY
   re-check → push → report.
