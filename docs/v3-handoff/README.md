# v3 handoff — live state on host `mike`, resumed 2026-09-28 15:20 EDT

The active plan is `roost-v3-finish-and-cutover-plan.md` in this directory;
every other file here is a brief or lead note it cites (`local://` paths from
the original session were rewritten to `docs/v3-handoff/`). The plan wins on
any conflict. `RESTART-PROMPT.md` is the start-from-nothing brief that moved
the work onto this host; it is spent — this file is the live state.

## This host

`mike`, Linux x86_64, 8 cores, 62 GiB RAM, 915 GB NVMe with ~748 GB free.
Disk stopped the previous host at 111 GiB; here a per-track target dir runs
4–7 GiB because `~/.cargo/config.toml` is a copy of `host-cargo-config.toml`
in this directory (dependency debuginfo off, zlib-compressed debug sections).
The workspace `Cargo.toml` also sets dev/test `debug = "line-tables-only"`.

`roost-build-slot` and `roost-target-sweep` are installed in
`/home/mike/repos/` (copies in this directory; `dir=` already edited for this
host). The slot script admits three cargo builds host-wide, one per track.
Every cargo/dx command is wrapped:
`flock <worktree>/target-track/.roost-build.lock /home/mike/repos/roost-build-slot <cmd>`,
and `/home/mike/repos/roost-target-sweep <worktree>/target-track` runs under
the same lock after every gate. `cargo clean` a track's `target-track` if it
passes ~12 GiB. Never set `RUSTFLAGS` in a build env (it replaces the host
linker flag). The integrator's `target-gate` exists only during a merge gate.

**This host is a live production v2 worker.** `roost-worker.service` (label
`desktop-pc`, bun, door 127.0.0.1:4104) runs out of
`~/.local/share/RoostWorkerV2/service/releases/worker/96f4db25…` against the
production coordinator `https://mike.roosttt.com`, and its multiplexed keeper
holds live PTYs. Never kill or restart either, never write inside that data
dir, never bind or probe 4104. v3's local door is 4114; Playwright stacks pick
their own free ports.

Worktrees: `roost-v3` (integrator, `v3`), `roost-v3-coord` (`v3-coord`),
`roost-v3-worker` (`recover/workerroot`), `roost-v3-web` (`v3-web`),
`roost-v3-trial` (detached, the integrator's trial merge), `roost-v3-web-gate`
(detached clean checkout of `v3-web` for gate runs).

## Environment finding: the door page loads as a download on this host

`smoke/terminal/terminal-local-fast-path.spec.ts` (chromium-serial) fails at
`page.goto("http://127.0.0.1:<door>/#pair=…")` with Playwright's
`Error: goto: Download is starting`, and `terminal-peer.spec.ts:61` fails the
same way. Measured twice on 2026-09-28 16:20: once with
`ROOST_SMOKE_COORD_EXECUTABLE` set to the Rust coordinator and once with it
unset (the whole TypeScript stack) — **identical error both times**, so it is a
host/environment fault, not a v3 port defect. v2's SPA responder
(`packages/host/src/spa.ts:127-166`) serves `/` as `index.html` with
`text/html; charset=utf-8` and the worktree's `apps/web/dist` is complete, so
the rejected response is not the index path. Stage 3.2 runs the whole terminal
oracle on this host, so diagnose it there with a `curl -I` against a live
stack's door before the first full run; whatever answers, the Rust door
(`roost-worker`'s W-DOOR slice) must not reproduce it.

## Cross-track defect: the worker's boot-time Connect call is unauthenticated

Found by the integrator on the merged `v3` at `11e73966` while closing the two
gaps the worker handoff flagged. One is stale, one is real.

**Stale — `recovery_metadata` is filled.** `SessionsList` answers it, but only
for a worker principal: `crates/roost-coord/src/sessions/list_projection.rs:107-126`
(`SessionListScope::OwnWorkerRecovery { worker_fp }`) into
`rpc_sessions.rs:84`, with `tests/sessions_list_auth.rs:194`
(`a_browser_lists_sessions_but_never_their_recovery_metadata`) pinning that a
browser never sees those rows. Nothing to do.

**Real — the worker reads its open-session set without a credential, and the
coordinator refuses that caller.** The chain:

- `crates/roost-coord/src/rpc/method_route_rows.rs:79` — `SessionsList` is
  `AuthRequirement::DeviceOrOwnWorkerRecovery`. An absent credential is a
  refusal, not an anonymous answer.
- `crates/roost-worker/src/runtime/boot_sequence.rs:85` — one client is built
  for every boot-time call, and its own comment says it is "reused by the
  open-session read below". `activation::coordinator_client`
  (`bootstrap_redeem/activation.rs:83-100`) attaches nothing to it.
- `crates/roost-worker/src/runtime/boot_sequence.rs:135` hands that client to
  `reconcile::admit_keeper`, which calls `client.sessions_list(request)`
  (`reconcile.rs:84-87`).

So boot step 5 — the read that decides keeper admission — is refused by the v3
coordinator, and a Rust worker cannot complete boot against it. The worker
already holds the credential everywhere else: enrollment passes one explicitly
(`activation.rs:58-59`) and the link dial gets `WorkerKeyCredential`
(`boot_sequence.rs:252-253`). The fix is to attach that credential to the
Connect client the way the dial does, not to relax the route. Fix it in the
worker track after its merge into `v3` and before the workspace gate; no gate
criterion would otherwise catch it, because nothing in `cargo test` crosses
the two processes.

## RESUME STATE — read this first

Three track leads run in parallel, one per track worktree, each owning its
branch and its gate; the integrator owns `v3`, every merge and the workspace
gate. A snapshot is a `git stash create` commit: restore it with
`git checkout <branch> && git stash apply <snapshot>` on the base named in the
table. Each track's own handoff doc carries the per-slice state — read it
before touching that track.

| Track | Branch tip | Uncommitted → snapshot | Track handoff |
|---|---|---|---|
| integrator | `v3` = `4e2b002b` | `v3-integrator-snap-trialmerge` = `67312bd3` → `a116afe7` (the trial merge plus three uncommitted conflict fixups) | this file |
| coord (Stage 2C) | `v3-coord` = `0fd87e56` (local only, `origin/v3-coord` = `8196fc88`) | `v3-coord-snap-resume2` = `f88552d7` → `0fd87e56` (the six mutants the dead subagent left applied; reverted in the worktree, kept here as evidence) | `coord-lead-handoff.md` |
| worker (Stage 2W) | `recover/workerroot` = `v3-worker` = `7a46b99a` | `recover/workerroot-snap-resume2` = `0865947d` → `7a46b99a` (the whole uncommitted wave 2+3 series: 153 modified + 250 untracked). Older: `…-snap-resume1` = `fe96f411` (13:40), `…-snap-drafts2` = `9e64ae3b` (`target-track/pause/` drafts and mutation artefacts) | `worker-lead-handoff.md` "RESUME STATE" |
| web (Stage 5) | `v3-web` = `28eebcee` | `v3-web-snap-resume2` = `dabfd789` → `28eebcee` (10 modified + 5 new files: the terminal stream probe, a diagnostics RPC call, pane registry/surface) | `web-lead-handoff.md` "Current state" |

**The session that ran here 10:45–14:50 on 2026-09-28 died mid-flight.** Its
`omp` process (pid 1487922) ended with no panic, nothing in the kernel log and
no error in its own log, with a coord subagent applying mutations; the last
file write anywhere was 14:50. Everything on disk survived. The integrator
reverted the six applied mutants in the coord product tree and snapshotted all
four dirty worktrees to origin (the table above) before touching anything.

Where each track stands:

- **Coord.** Waves C-B, C-INPUT, C-SEND, SY3, C-SCREEN, S4, C-DIRECT, AT, GS,
  D1, C-RETAIN, C-BOOT and wave 4 (AG2, C-PUSH, X2) committed; ratchet
  `PortStatus::AwaitingDomainPort` rows = **0**, `#[ignore]` = 0. C-CAPTURE is
  committed locally as `0fd87e56` with a placeholder body and its mutation
  evidence lost. `terminal-render` 5/5, `terminal-delivery` 4/4, attachment-direct
  3/3, global-search 1/1 against the Rust coordinator. One open product defect:
  after `__smoke.pauseSyncTransport()`/`resumeSyncTransport()` every new sync
  socket is closed 1008 `invalid_client_frame` (`terminal-peer.spec.ts:209`).
- **Worker.** Wave 1 committed and gated (1223/0/0 ×2, clippy 0, lint 0). Waves
  2–3 — 13 slices, all wired and compiling — are **uncommitted**; a 57-mutant
  run at resume killed 56. Owed: the series as per-slice commits, the mutations
  never run (WAgentsDetect, WAttach, WCapture, WDoorHttp), the WKUpdate K-M1b
  survivor, the v2-map audit, the live door check, the CellCadence
  unattached-sink inference. Cross-track gaps: the worker Connect client sends
  no worker credential; the v3 coord does not fill `recovery_metadata`.
- **Web.** U-0, DECODE, PUMP, wave A, renderer core and wave B committed; gated
  tip `659b50c8` was 1333/0/4 twice, clippy 0, lint 3958 inputs 0, wasm32 clean,
  TERM gate 1 passed, production `dist` `__smoke` 0. Two commits landed after it
  (`32120c5e`, `28eebcee`) and a WIP is uncommitted. Not done: both dx bundles,
  the delivery spec on `dist-smoke`, and every U-2 row.
- **v3 itself** is still the Stage 0 merge (S3.0 green at `78d5dc24`) plus docs.
  No track work after Stage 0 has been merged into it; the trial merge in
  `roost-v3-trial` reached `a116afe7` (worker `7a46b99a`, coord `8196fc88`,
  web `c6f7af07`) and is stale against the tips above.

Rulings made during the run (all in the tracks' commit bodies): parity = v2 wins, with ONE user-visible exception — resumed direct uploads append at `bytesWritten` (v2 wrote at offset 0, corrupting the file). Deliberate path deviations: worker agent-report endpoint and attachment base live in the v3 worker data dir. `bun_abi` restored on `KeeperContractV1` with the Rust keeper reporting `"rust"`. `terminal_metadata_v1` (underscores) is v2's spelling. Public auth routes = v2's 7 exactly. `SmokeApi` has 53 members.

Operating lessons (put them in every lead brief): a lead that ends a turn while waiting dies (forced tool choice → API 400) — block in the foreground instead; leads run out of request budget in 1.5–3 h, so scope each lead to what one budget finishes and commit after every gated item; one build lock per worktree serializes that track's helpers — keep ≤3 helpers per lead, one level deep; sweep stale test binaries after every gate.

## Stage 0 state (plan "### Stage 0") — COMPLETE (history; the RESUME table above is current)

| Branch | SHA | State |
|---|---|---|
| `v3-worker` (= `recover/workerroot`) | `cceaf15a` | DONE. F6+F13 mutation confirmed; five over-cap files split; origin/v3 merged; 26 pre-existing red tests classified by v2 and fixed. Gate: `cargo test -p roost-worker -p roost-keeper` 603/0/0 twice, clippy 0, lint 0 (2492 inputs), fmt clean |
| `v3-coord` | `f3a717a6` | DONE: `files.rs` split + 8 arms (AwaitingDomainPort 27→19), send-queue ignore retired (0 `#[ignore]` in roost-coord), `event_snapshot_cap.rs`, 2 clippy fixes, origin/v3 merged. Gate: `cargo test -p roost-coord --no-fail-fast` 113 binaries 712/0/0 twice, clippy 0, fmt clean; lint 8 = roost-keeper only (cleared by the worker merge) |
| `v3-web` | `10822254` | U-0 items 1–3 DONE. Gate: 613/0/3 twice (the three named ignores), clippy 0, fmt clean, wasm32 build 0; lint 8 = roost-keeper only, cleared by the worker merge |
| `coord-guard-move-snap` | `73cf9322` | Snapshot of `roost-v3-coord-split`'s one untracked file (`recover-held-rollout.ts`) |
| `v3-web-snap` | `17464bb5` | SUPERSEDED (its DECODE work is committed in `dcfd84bb`) |
| `v3` | `78d5dc24` | All three tracks merged; S3.0 GREEN (see `docs/v3-gate-baselines.md` "Phase gates"); merged back into each track |
| `coord-guard-move` | `f7ff7e37` | Pushed; merged into no track. Inspect before deleting |

## Next steps, in order

1. The three leads close their tracks to a gated, pushed tip: coord (C-CAPTURE
   evidence, the `:209` Sync-resume defect, `terminal-local-fast-path`, header
   audit + README, track gate), worker (land the wave 2+3 series per slice, the
   owed mutations, K-M1b, v2-map audit, door check, track gate), web (finish the
   WIP, gate the tip, both bundles, the delivery spec, then U-2 with PAIRING
   first).
2. Integrator: trial-merge the three gated tips into `v3` in the order worker,
   coord, web, run the workspace gate (two agreeing `cargo test --workspace
   --no-fail-fast` runs, clippy 0, `cargo xtask lint` 0 with its input count,
   fmt clean), record it in `docs/v3-gate-baselines.md` "Phase gates", then
   merge `v3` back into each track branch.
3. Stage 3 backend gates (`roost-v3-finish-and-cutover-plan.md` §Stage 3) run
   here; Stage 3.3 (install gate as a scratch user) and Stage 4 (production
   cutover) are runbooks for the coordinator host.

## Host facts the plan relies on

- Production `https://mike.roosttt.com` (the v2 coordinator) runs on the
  original host; this host is a v2 **worker** in that fleet. Stages 3.3–4 touch
  the coordinator host, not this one.
- Port 4114 (v3 local door) is free here; v2's door on this host is 4104 and is
  in use by the live worker.
