# Coord lead handoff — Stage 2C, rows at 0

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
`git add` only (git identity is set in the repo config). JS: `bun install`;
`apps/web/dist` must be a SMOKE build (`VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`,
untracked). One spec against the Rust coord: `ROOST_SMOKE_COORD_EXECUTABLE=$PWD/target-track/debug/roost
ROOST_TEST_BUN=$(command -v bun) bun node_modules/@playwright/test/cli.js test
--config=playwright.config.ts --project=chromium-desktop --workers=1 smoke/terminal/<spec>`
(`@serial` specs such as terminal-local-fast-path need `--project=chromium-serial`).
Copy the `roost` binary aside before a spec if builds may run meanwhile.
After every track gate run `/home/mike/repos/roost-target-sweep <target-track>` under the
build lock.

Trap: a build-script binary compiled in another checkout path that shared this
target dir bakes that path in (`roost-proto` build.rs `env!("CARGO_MANIFEST_DIR")`);
if a build panics reading `/home/mike/repos/roost-v3-coord-verify/...`, delete
`target-track/debug/{build,.fingerprint}/roost-proto-<hash>` under the lock.

## Committed on v3-coord

Wave 3 (`14f076df` … `90a293a1`): cherry-picks `5208e0f9`/`7a46b99a`, RenderHistory
stream id, `09f1b60c` fleet_update → roost-protocol (integrator cherry-pick candidate),
C-RETAIN, C-DIRECT, AT, GS, D1.

Wave 4 (this session):

| SHA | What |
|---|---|
| `f83fc63e` | rustfmt + clippy on wave-3 files |
| `cb49dd18` | AG2 SessionsPrompt (row → Implemented) |
| `57ed3b36` | C-PUSH production Web Push transport + viewer suppression |
| `84467999` | X2 DiagSnapshot wired (row → Implemented) |

**Ratchet: `PortStatus::AwaitingDomainPort` rows = 0.** `#[ignore]` in roost-coord = 0.

## Spec evidence (Rust coord debug build at `84467999`, TS worker, smoke dist)

- terminal-render 5/5.
- terminal-peer 3/5 run: `:96` and `:175`, `:298` pass; `:61` fails `goto: Download is
  starting` on the worker's loopback door — identical with the TS coordinator
  (environment); `:209` is the open Sync-resume defect below.
- earlier: attachment-direct 3/3, global-search 1/1. terminal-local-fast-path not run.

## Open defect: Sync resume closes 1008 `invalid_client_frame` (terminal-peer `:209`)

After `__smoke.pauseSyncTransport()`/`resumeSyncTransport()` every new socket is closed
by `sync_ws/ingress.rs` with `invalid_client_frame` 1 ms after the terminal domain is
seeded (`domain_seeded admitted=1`, 66 unacked bytes). Passes with the TS coordinator.
Every Invalid path logs the same reason: decode/canonical (`is_canonical_client_frame`),
`apply_ack` over `last_sent_seq` (`commands.rs:178`), an empty frame, an unknown domain,
or a failed terminal reset. v2 `sync-ws-client-ingress.ts` has the same checks, so find
which one fires (log the path + ack/last_sent + frame hex) before changing anything.

## UNCOMMITTED — C-CAPTURE, snapshot `origin/v3-coord-snap-capture` (`215a8f9c`)

`crates/roost-coord/src/terminal_capture/*` (restore: `git stash apply 215a8f9c`; then
add `pub mod terminal_capture;` to lib.rs). Ported, NEVER COMPILED, no tests. Defines
command/result (`TerminalCaptureCommand`, `TerminalCaptureResult`, action names) and
coordinator record/section types (`TerminalCoordinatorRecord`, `TerminalCoordinatorSection`,
payload) that `roost_protocol::terminal_capture` lacks; v2 keeps them in
`packages/protocol/src/terminal-capture.ts`, and the ruling puts capture types in
roost-protocol — move them there as a self-contained commit and tell WorkerLead.
Wiring (not applied): `CoordServices.terminal_capture: Arc<TerminalCaptureRuntime>`;
`diag_snapshot.rs` capture branch → `CaptureBridge{..}.handle(capture, &caller.principal)`;
recorder hook after `residency.replace` in `terminal_screen/replica_admission.rs`
(`install_cache`, `accept_delta`; v2 terminal-screen-hub.ts:323,:373). Tests to port:
terminal-capture-bridge.test.ts (15), terminal-capture-recorder.test.ts (10).

## Next steps, in order

1. terminal-peer `:209` Sync-resume defect.
2. C-CAPTURE (types → roost-protocol, compile, tests, wiring, mutations).
3. terminal-local-fast-path (chromium-serial); header audit; track gate; release
   live-stack READY check; report.
