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

## UNCOMMITTED — snapshot `origin/v3-coord-snap-wave4` (`94511587`), NOT gated together

Restore: `git checkout v3-coord && git stash apply 94511587`. Contents:

- AG2 (`agents/{prompt_control,rpc_prompt}.rs`, `workers/terminal_send.rs` agent-prompt
  sender, `terminal_input/write_control.rs` `WorkerWritten`, tests `agent_prompt_*` 13/13,
  5 mutations seen to fail): DONE; `SessionsPrompt` arm + row applied (ratchet 2 → 1).
- C-PUSH (`push/{transport,viewers,dispatch,mod}.rs`, `serve.rs` installs push delivery,
  `NoTerminalViewers` deleted, tests `push_*`, `agent_status_retirement`): DONE per helper,
  mutations seen to fail. The fold already deleted on retirement (`status_hub.rs:263-273`).
  Lead to-do: fix the stale doc at `agents/status_push.rs:55-57` (text in
  `agent://CoordLead.CPush`).
- X2 (`diagnostics/{diag_snapshot,worker_results,session_state}.rs`, `workers/diag_send.rs`,
  tests `diag_snapshot_*` 16/16): handler done, NOT wired. Lead to-do: `CoordServices.
  diag_pipelines: Arc<WorkerTerminalPipelineSnapshotCache>` (built from `scrollback`), the
  `diag_snapshot` arm calling `handle_diag_snapshot(&core, caller, &services.diag_pipelines,
  &self.git_sha, req)`, row → Implemented; mutations at diag_snapshot.rs:51,163,247,
  worker_results.rs:103, session_state.rs:94, diag_send.rs:202 still to be SEEN to fail; log
  the malformed-fp drop in diag_snapshot.rs. Then terminal-render 5/5 and terminal-peer `:96`.
- C-CAPTURE (`terminal_capture/*`, `pub mod terminal_capture;` in lib.rs): ported, NEVER
  COMPILED, no tests. Reuses `roost_protocol::terminal_capture` (7b19778b) but defines
  command/record types the protocol lacks — check that against the worker's types before
  keeping. Wiring (services field, diag_snapshot capture branch, recorder hook in
  `terminal_screen/replica_admission.rs`) in `agent://CoordLead.AG2Prompt`.

## Open defect: Sync resume closes 1008 `invalid_client_frame` (terminal-peer `:209`)

After `__smoke.pauseSyncTransport()`/`resumeSyncTransport()` every new socket is closed by
`sync_ws/ingress.rs` with `invalid_client_frame` right after the terminal domain is
seeded (coord log in the trace). Passes with the TS coordinator. Either a non-canonical
decode (`is_canonical_client_frame`, buffa vs protobuf-es re-encode) or `apply_ack` over
`last_sent_seq`; capture the offending frame bytes to tell which.

## Next steps, in order

1. Restore the snapshot; `cargo check -p roost-coord --all-targets`; fix C-CAPTURE.
2. Commit AG2, C-PUSH, X2 (wired) each by path with evidence; C-CAPTURE with ported tests
   (terminal-capture-bridge.test.ts 15 cases, terminal-capture-recorder.test.ts 10).
3. Specs: terminal-render 5/5, terminal-peer, terminal-local-fast-path (chromium-serial).
4. Header audit; track gate (two agreeing runs, clippy, lint, fmt — the wave-3 files have
   rustfmt drift, e.g. direct_results.rs, live_frames.rs, deploy/*); push; report.
