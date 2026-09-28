# Coord lead handoff — Stage 2C, PAUSED mid wave 3 (user moving machines)

**A moment, not a state.** Read `git status --porcelain` and `git log --oneline -15`
in the coord worktree before trusting any line below. The plan
(`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2C") wins.

## Build environment

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 \
       CARGO_TARGET_DIR=<worktree>/target-track
flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …
```

Never end a turn to wait (API 400 crash); block in the foreground. Every commit uses a
path-restricted `git add` (slices share the tree). JS: `bun install`; `apps/web/dist` must
be a SMOKE build (`VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`, untracked). One spec
against the Rust coord: `ROOST_SMOKE_COORD_EXECUTABLE=$PWD/target-track/debug/roost
ROOST_TEST_BUN=$(command -v bun) bun node_modules/@playwright/test/cli.js test
--config=playwright.config.ts --project=chromium-desktop --workers=1 smoke/terminal/<spec>`.
After every track gate run `/home/almalinux/repos/roost-target-sweep <target-track>` under the
build lock; `cargo clean` between waves when target-track > ~12 GiB.

## Committed and pushed (origin/v3-coord)

| SHA | What |
|---|---|
| `3ab53db1`, `e316737f` | merges of worker `29efc28e`, `deda6301` (view registry → roost-protocol) |
| `466d74d8` | C-BOOT: authorized-keys import at boot, FK validation, pre-migration backup |
| `7ff8f75e` | C-B: WL-WIRE + SY2 + capability `terminal_metadata_v1` (compiles only with `6d72521c`) |
| `2327f074` | AuthRedeemWorker/Browser public as v2 |
| `6d72521c` | S4 sessions: 7 rows (19 → 12) |
| `a07f9ead`, `1fdc090a`, `b37b744b` | shared layout adapter (from v3-web); coord `ui_state/layout_proto.rs` deleted |
| `fa50bcf9`, `638fb4dc` | clippy fix; handoff |
| `c9772f99` | protocol: KeeperContractV1 `bun_abi` (keeper reports `KEEPER_RUNTIME_ABI` "rust") |
| `5d9d9001` | C-INPUT + C-SEND + SY3 + C-SCREEN; SessionsInput (12 → 11) |
| `c96a21a0` | handoff after wave 2 |
| `337437b2` | `crates/roost-coord/README.md` with the not-ported-by-decision table |

Last gated tree `5d9d9001`: roost-coord+roost-protocol 188 binaries 1247/0/0, clippy
workspace 0, lint 3115 inputs 0 violations. C-B check at `fa50bcf9`: release live-stack
`READY http://127.0.0.1:32921 worker=f9d8bb6d…`. Specs (Rust coord): terminal-delivery 4/4,
terminal-render 3/5.

**Committed ratchet: AwaitingDomainPort 11; `#[ignore]` 0.**

## UNCOMMITTED wave 3 — snapshot `origin/v3-coord-snap-pause` (NOT gated, NOT compiled together)

Restore: `git checkout v3-coord && git stash apply <sha of refs/heads/v3-coord-snap-pause>`.
In that tree `AwaitingDomainPort` = 2 (only `SessionsPrompt`, `DiagSnapshot` left) because
slices flipped their rows before gating — every flip must be re-verified by the gate.

| Slice (agent) | Owns | State at pause |
|---|---|---|
| RenderHistory | `sync_ws/terminal/*`, `terminal_screen/*` | Root cause of the 2 terminal-render failures found and fixed (NOT the dropped view-stream controller): `pump_delta` queued deltas without `terminal_stream_id` in their meta, so `advance_cursor` never matched and the lane stayed in-flight after its first delta; also double-removal on delivery and refused-delta drop. Specs not re-run. The off-bottom reader, foreground-liveness and frame-repair specs also need `__smoke.terminalStreamProbe` → **DiagSnapshot (X2)** |
| CDirect | new `terminal_direct/*`, rows GrantLocalTerminal / NegotiateLocalTerminalPeer | in progress; tests `tests/terminal_direct_*` |
| AtAttach | `attachments/*`, 3 rows | in progress |
| GsSearch | `search/*`, 2 rows | in progress; tests `tests/search_*` |
| D1Deploy | `deploy/*`, 2 rows, UpdateProgress arm | in progress. Pending decisions for the integrator: tokio `process` feature added to `crates/roost-coord/Cargo.toml` (deploy jobs spawn `roost deploy`); catch-up uses a local 3-state `workerUpdateState` (the full one is a roost-cli stopgap, `roost-cli/src/status/update_state.rs`); probes v3 `fleet-push-journal.json` instead of v2 `transactions/coordinator-deploy.json`; UpdateProgress's only v2 consumer is Windows → warn refusal |
| CRetain | `worker_link/{announced_*,connection,link_session,frame_dispatch,handshake,result_lane,frame_queue}.rs` | in progress: rewiring the barrier into the read loop with a payload-carrying machine (v2 shape) and a socket-wide retained-work budget |

Slice agents' own final reports (if they yielded before the pause) are at
`agent://CoordLead2C.<Name>` in the original session.

## Merges waiting

- Worker `5208e0f9` (roost-protocol only): agent-status `active=false` retirement decode —
  cherry-pick before AG2 / C-PUSH; the coord fold must delete on retirement (v2
  `agent-status-hub.ts:136-140`).
- Worker terminal_capture protocol types: not landed yet (worker `recover/workerroot-snap-pause`);
  C-CAPTURE reuses them.

## Next steps, in order

1. Restore the snapshot; `cargo check -p roost-coord --all-targets`; route errors by owner file.
2. Finish each wave-3 slice (a fresh agent per slice with "continue from the files in the
   worktree" + its row list above); run its tests, mutations and named spec
   (terminal-render 5/5; terminal-local-fast-path, terminal-peer; attachment-direct;
   global-search); then gate (two agreeing runs, clippy, lint, fmt) and commit per slice by path.
3. X2 (DiagSnapshot, unblocks terminalStreamProbe specs) + C-CAPTURE; AG2 + C-PUSH after
   cherry-picking `5208e0f9`.
4. Header audit: `/tmp/coordlead-coverage.sh` (v2 basename in a `//!` line or README) was 64
   uncovered at `5d9d9001`; wave 3 + X2/C-CAPTURE/AG2/C-PUSH cover most; the rest are header
   gaps for already-ported files (agent-status-order, worker-agent-status-frame, self-hosted-
   tenant uses `//` not `//!`, db/snapshot, gzip-file, bun-handler, handlers-streaming,
   handlers-tasks, worker-conn-types, worker-frame-queue, worker-service) — verify each is
   really ported before naming it.
5. Track gate → release live-stack READY re-check → push → report.
