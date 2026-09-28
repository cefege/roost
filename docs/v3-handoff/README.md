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

## RESOLVED: the door page loaded as a download — a harness bug, not this host

`terminal-local-fast-path.spec.ts` and `terminal-peer.spec.ts:61` both failed at
`page.goto("<door>/#pair=…")` with `Error: goto: Download is starting`, on the
**worker-served** page; the coordinator page in the same test enrolled normally.
The identical failure under the Rust and the TypeScript coordinator is what
rules out a host fault — a real one does not survive a stack swap.

Chromium was reporting the truth: the door answered `404` with
`content-type: application/octet-stream`, so a document navigation is a
download. `smoke/terminal/stack-worker-runtime.ts` set `ROOST_WEB_DIST_PATH` for
the coordinator child and for nobody else, and
`packages/host/src/web-embed.generated.ts` is an empty stub outside a release
build, so the worker's door had neither a disk build nor embedded assets. Both
launchers now resolve the key identically. Fixed on `v3` at `a84f4a12`:
`terminal-local-fast-path` 1 failed → 1 passed, `terminal-peer` 4/5 → **5/5**.
The entry is `docs/FAILURE-INDEX.md` ("A smoke spec dies at enrollment with
'goto: Download is starting'"). `origin/main` carries the same omission, which
is why v2's local-door specs lean on a checkout-local `.env`.

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
Connect client the way the dial does, not to relax the route. No gate
criterion would otherwise catch it, because nothing in `cargo test` crosses
the two processes.

**Fixed on `v3` at `c92d793f`, and the trial merge then found a second one.**
`bootstrap_redeem::authenticated_call_options` mints the key and attaches it as
a `Bearer` header with `try_with_header`, `register.rs` routes through that one
function instead of spelling the header a second time, and
`tests/open_session_credential.rs` serves a real Connect server that answers
`SessionsList` only to a caller presenting a bearer — the defect is a header on
the wire, so a fake transport passes with or without the fix. Mutation, seen to
fail and reverted: returning bare `boot_call_options()` with no header made the
read fail with `unauthenticated: sessions.list requires a worker credential`.

The worker series, merged afterwards, brought `CoordinatorOpenSessions` — a
long-lived `OpenSessionSource` that holds the same bare client and calls
`read_open_sessions` again after boot. It was refused for the same reason, and
no gate run on either branch separately would ever have seen it: the two call
sites live on opposite sides of the merge. The merged tree gives that struct an
`Arc<dyn CredentialSource>` and `boot_sequence` hands it the same key.

## RESUME STATE — read this first

Three track leads run in parallel, one per track worktree, each owning its
branch and its gate; the integrator owns `v3`, every merge and the workspace
gate. A snapshot is a `git stash create` commit: restore it with
`git checkout <branch> && git stash apply <snapshot>` on the base named in the
table. Each track's own handoff doc carries the per-slice state — read it
before touching that track.

| Track | Branch tip | Uncommitted → snapshot | Track handoff |
|---|---|---|---|
| integrator | `v3` = `c92d793f` | none — the tree is clean; the trial merge is superseded by the real merges | this file |
| coord (Stage 2C) | `v3-coord` = `1888d9c5`, **merged into `v3`** at `7ef53cfb` | `v3-coord-snap-resume2` = `f88552d7` (the six mutants the dead subagent left applied; reverted, kept as evidence) | `coord-lead-handoff.md` |
| worker (Stage 2W) | `recover/workerroot` = `v3-worker` = `91dd7656` | none needed — the worktree is clean. `recover/workerroot-snap-resume2` = `0865947d` still holds the pre-series state | `worker-lead-handoff.md` "SERIES_SHAS" / "OPEN" |
| web (Stage 5) | `v3-web` = `654bd72a`, **merged into `v3`** at `11e73966` (gated tip `f17b305c`) | `v3-web-snap-u2a` = `d28d7a5a` → `654bd72a` (13 files of in-flight U-2) | `web-lead-handoff.md` "Current state" |

**The session that ran here 10:45–14:50 on 2026-09-28 died mid-flight.** Its
`omp` process (pid 1487922) ended with no panic, nothing in the kernel log and
no error in its own log, with a coord subagent applying mutations; the last
file write anywhere was 14:50. Everything on disk survived. The integrator
reverted the six applied mutants in the coord product tree and snapshotted all
four dirty worktrees to origin (the table above) before touching anything.

Where each track stands:

- **Coord — DONE and merged.** Every `AwaitingDomainPort` row has an arm (0
  rows), `#[ignore]` = 0, 204 of 204 v2 `apps/coord/src` non-test modules named
  by a `//!` header or listed in `crates/roost-coord/README.md`, and C-CAPTURE
  is committed with its 26 ported tests. The "open Sync-resume defect" was log
  blindness, not a defect: five client-frame checks logged one reason, and
  `367c139d` had already fixed the real cause — `terminal-peer.spec.ts:209` is
  green. `d6395a97` gave each check its own reason; `1888d9c5` fixed the three
  `terminal_input_sync` reds that same blindness had hidden. Specs against the
  Rust coordinator: terminal-render 5/5, terminal-delivery 4/4, terminal-peer
  4/5 (`:61` is the environment failure above), attachment-direct 3/3,
  global-search 1/1.
- **Worker — series landed, track gate running.** Waves 2–3 are 16 per-slice
  commits (`3901daa9` … `e565f760`) plus the handoff; the tree type-checks at
  `e565f760`. The owed mutation batches are RUN: 31 of 40 killed, with nine
  survivors that need a guard not yet written (WAttach A1, A2; WCapture CC1, CC2,
  CC4, CC7, CC8; WAgentsDetect M10, M12) and WDoorHttp D1b classified as
  equivalent-and-masked. K-M1b and the CellCadence boot-state inference are both
  RESOLVED — the first was a broken oracle (`pgrep -f` counts a fork-to-`exec`
  window, not an identity), the second is v2 parity. Still open: the track gate,
  the live door check (needs a release binary), 13 files of the header audit,
  and those nine survivors. Both cross-track gaps it flagged are closed: the
  credential is `c92d793f`, and `recovery_metadata` is filled.
- **Web — merged at its gated tip, U-2 in flight.** U-0, DECODE, PUMP, wave A,
  renderer core and wave B are committed; the gated tip is `f17b305c` (nextest
  1344 passed / 4 skipped twice and agreeing, 387 passed with `--features smoke`,
  clippy exit 0, lint 3985 inputs 0 violations, fmt clean, wasm32 clean with and
  without the smoke feature), and `v3` carries it. That tip's slice is the
  terminal diagnostic probe: `__smoke.terminalStreamProbe` crosses client-core
  into coord's X2 `DiagSnapshot` and back. Three fixes rode with it, named in
  the commit: a `string_member` that did not exist, a harness still answering
  `unported_refusal` for a now-ported member, and a vacuous probe test whose
  fixture named no session. U-2 (PAIRING first) is running in the worktree.
- **v3 itself** now carries the coord merge (`7ef53cfb`), the web merge
  (`11e73966`) and the cross-track credential fix (`c92d793f`). The trial merge
  in `roost-v3-trial` is superseded; the workspace gate has not been run on
  this tree yet.

## The worker merge, resolved and type-checked in the trial worktree

`v3` does not yet carry the worker's 16 slice commits. The integrator rehearsed
that merge in `roost-v3-trial` (detached, `target-trial/`), because the
credential fix and the worker series touch the same files. It produced exactly
three conflicted files and **two defects no single-branch gate could see**:

| file | resolution | why |
|---|---|---|
| `Cargo.toml` | keep both sides | additive: web added `unicode-segmentation`, the worker added `unicode-normalization` for the integration installer's NFC path compare |
| `crates/roost-protocol/src/terminal_capture.rs` | keep `v3` | coord made it a directory module with `command` + `coordinator`; the worker's single file is a strict subset and its submodule files are present |
| `crates/roost-worker/src/runtime/reconcile.rs` | combine | the worker changed the return type to `OpenSessionSet` (rows + validated recovery references) while the credential fix added a parameter — both are needed |

Then two follow-on edits the conflict markers do not show, because each side's
hunks merged cleanly and were wrong together:

- `crates/roost-keeper/src/keeper.rs` — the merge emitted
  `use roost_protocol::keeper_update::KEEPER_RUNTIME_ABI;` **twice** (E0252).
  Delete one. This is the same fix the first trial merge needed, which is why
  the stale fixups were snapshotted rather than trusted.
- `crates/roost-worker/tests/open_session_credential.rs` — the new guard
  asserted `rows.len()` on a value that is now an `OpenSessionSet`; it is
  `open.rows.len()`.

And one thing to carry across deliberately, not a conflict at all:
`CoordinatorOpenSessions` (the worker's post-boot `OpenSessionSource`) holds the
same bare client and calls `read_open_sessions` again. It needs an
`Arc<dyn CredentialSource>` field, and `boot_sequence.rs` passes it the same
`WorkerKeyCredential` the link dial uses.

Re-run the rehearsal after the worker's next push — its tip moves.

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
