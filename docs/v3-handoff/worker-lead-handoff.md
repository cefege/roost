# Track 2W lead handoff — `recover/workerroot` (pushed to `v3-worker`)

Leads: `WorkerLead2W` (Stage 2W of `roost-v3-finish-and-cutover-plan.md`), then
`WorkerLead` (series landing, mutations, close-out). Earlier leads' notes are in
git history of this file.

## State

The wave 2+3 series is committed, per slice, in dependency order. Every slice
commit was type-checked with
`cargo check -p roost-worker -p roost-keeper -p roost-protocol --all-targets`
before the next one landed; two commits were re-cut after a check named a file
a later slice owned (`tests/cell_cadence.rs` and the `TableChannelDelivery`
call site), so WPeer's commit carries `cell_cadence.rs`.

## SERIES_SHAS (`recover/workerroot` = `origin/v3-worker`)

| SHA | slice |
|---|---|
| `3901daa9` | ProtoDoor — protocol only: local-UI-door paths, subprotocol, payload limits, allowed origins |
| `be1417dc` | ProtoAttach — protocol only: attachment transfer contract, peer packet framing, send queue |
| `d73e76df` | WResume — survivors adopt from keeper history, respawn, keeper death, retirement |
| `3ce97ae2` | WKUpdate — keeper-update admission, `keeperUpdatePrepare`, keeper tree reap |
| `bdc521ca` | WHeart — heartbeat, folder facts, stray reap, channel-creation gate |
| `2cee434d` | WDurable — durable session events, the snapshot as a sequenced Event frame |
| `3771f744` | WBootOrder — boot dials before reconciling, durable replay barrier, agent conversation restore |
| `76dd7e00` | WDoorHttp — loopback door: Host/Origin gate, bootstrap, upgrades, SPA |
| `8892344b` | WDoorTerm — local terminal door: grants, prehello, direct sockets, scrollback |
| `0cc88e73` | WPeer — WebRTC terminal peer, direct-terminal arms |
| `8c274e18` | WAttach — attachment store, durable operations, reaper, grants, coordinator arms |
| `8db2294e` | WAttachDirect — direct and peer carriers |
| `8f285aab` | WCapture — capture recorder, taps, segments, gzip bundle, validator |
| `eb1104f1` | WAgentsReport — agent report endpoint, protocol, server, screen manifests |
| `557f6f94` | WAgentsDetect — agent-status detection, registry, process scan, retirement outbox |
| `e3e71b30` | WAgentsInstall — OMP/Pi integration install (also the root `Cargo.toml` edge) |
| `b562145c` | WAgentsPrompt — agent prompts, conversation references, OMP resume restore |
| `1269b3b4` | close-out 1 — the reap-sweep guard pinned by pid, the CellCadence boot-state guard, the Windows-only README list |
| `e565f760` | close-out 2 — the v2-module header audit (74 files) |

Shared-file edits land whole in the first slice commit that lists them:
`Cargo.lock` in WCapture, root `Cargo.toml` in WAgentsInstall, the roost-host and
roost-keeper changes in WResume and WKUpdate. **The two protocol commits
(`3901daa9`, `be1417dc`) are protocol-only and safe to cherry-pick.**

## RESUME STATE (superseded by the tables below)

The pause/resume notes this section used to hold are in git history
(`0f79ffae`, `28e3cd1d`). What they listed as open is now:

| item | state |
|---|---|
| WKUpdate K-M1b (the SIGKILL sweep had no guard) | RESOLVED — `1269b3b4` |
| the CellCadence boot-state [INFERENCE] | RESOLVED, and it is v2 parity — `1269b3b4` |
| the Windows-only not-ported list | DONE — `crates/roost-worker/README.md` |
| the `//!` header audit | 74 of 87 files DONE — `e565f760`; 13 open, listed in that commit body |
| `link_drain.rs` catch-all arms | 0 (the ratchet number) |
| mutations owed (WAgentsDetect 13, WAttach 8, WCapture 9, WDoorHttp C+D1) | RUN — see MUTATIONS below |
| the track gate (tests x2, clippy, lint, fmt) | NOT RUN in this session; see OPEN below |
| the live door check | NOT RUN in this session; see OPEN below |

## MUTATIONS (this session; `mutbatch.py`, every file restored after each run)

Runner `/home/mike/wl/mutbatch.py`; specs `/home/mike/wl/muts-{doorhttp,attach,capture,wad}.json`;
logs `target-track/mut/{doorhttp,attach,capture,wad}.log`.

- **WDoorHttp batch C + D1** — C1 (origin gate) KILLED
  `a_configured_extra_origin_is_admitted_and_nothing_else_is`; C2 (index cache rule) KILLED
  `a_deep_link_is_the_uncached_shell_and_a_write_is_refused`; D1a (the upgrade's own
  `max_message_size`/`max_frame_size`) KILLED
  `a_terminal_frame_over_the_payload_ceiling_is_never_read`; **D1b (the attachment route's
  own size gate) SURVIVED — equivalent, and it is why the pair is masked**: both routes share
  one upgrade, so the upgrade's frame ceiling still closes an oversized attachment frame.
  The pair (both removed) is what fails
  `an_oversized_attachment_frame_closes_the_socket_before_its_owner_reads_it`, as WKUpdate's
  commit body records.
- **WAttach (8)** — A3 (unique name) KILLED, A4 (the manifest is never swept) KILLED,
  A5 (LRU eviction order) KILLED, A6 (only a coordinator-carrier operation is failed by the
  idle sweep) KILLED, A7 (`capability_matches`) KILLED, A8 (lease deadline min) KILLED.
  **A1 (one grant, one live carrier) and A2 (a chunk whose seq is not zero cannot open a
  missing operation) SURVIVED** — no guard distinguishes them today; see OPEN.
- **WCapture (9)** — CC3 (the write path reserves its slot) KILLED, CC5 (an expired lease is
  disarmed) KILLED, CC6 (a resize is noted at install) KILLED, CC9 (session close releases the
  recorder) KILLED. **CC1, CC2, CC4, CC7, CC8 SURVIVED** — see OPEN.
- **WAgentsDetect M1–M12 + P1 (13)** — M1, M2, M3, M4, M5, M6, M7, M8, M9, M11, P1 KILLED.
  **M10 (the agent-status lane in `link_drain.rs` is never drained) and M12
  (`agent_statuses.send(item, can_write_direct && false)`) SURVIVED** — the link's
  agent-status arm has no guard; M10 is not even reached by the `--lib` suite. See OPEN.

## The two questions this session settled

**WKUpdate K-M1b.** `sweep_survivors(&members);` -> `drop(members);` left
`a_nohup_job_that_ignores_sighup_is_still_reaped` green, and the cause was the
oracle, not the product: the test counted `pgrep -f "sleep 7761100"` matches, and
a pattern is not an identity — the three tests in that bin reap their own trees
concurrently, and pgrep counts the fork-to-`exec` window. Run alone with
`--test-threads=1` the same mutation failed in 6.59 s. The test now takes the
job's identity from the shell (`$!` is the subshell's pid and `exec` keeps it),
asserts it is still alive 300 ms after the kill is acknowledged, and the mutant
is KILLED. The unmutated bin is 3/3 green on two consecutive runs.

**The CellCadence boot-state [INFERENCE] — refuted, and v3 matches v2.**
`CellCadence::new` registers the coordinator sink at boot and nothing suspends
it until a link opens, so it is ACTIVE and unattached and its `send_frame`
answers `Dropped`. That costs a stream-wide repair full per delta — and v2 does
exactly the same: its sink is registered the same way
(`apps/worker/src/main.ts:210-217`) and `sendCellGrid` returns "dropped" whenever
the link is down (`apps/worker/src/transport/coord-link-outbox.ts:267-278`).
Local-door deltas do NOT stall: the local sink is a separate receiver and takes
every frame. New guard
`cell_cadence::a_boot_state_coordinator_sink_never_stalls_a_local_door_sink` pins
it: a delta delivered with no coordinator ever attached paints on the local sink
and is handed to the coordinator sink not at all.

## OPEN (plan order)

1. **Track gate.** Not run in this session: two agreeing green runs of
   `cargo test -p roost-worker -p roost-keeper --no-fail-fast`,
   `cargo clippy --workspace --all-targets -- -D warnings` = 0,
   `ROOST_REPO_ROOT=$PWD cargo xtask lint` = 0 (report its input count), and
   `cargo xtask fmt` clean per `git status --short`. The tree type-checks
   (`cargo check -p roost-worker -p roost-keeper --all-targets`, exit 0) at
   `e565f760`. Reason: the session's budget ran out after the mutation batches.
2. **Live door check.** Not run. A `curl` of the door on a Playwright stack port
   (never 4104, never 4114) needs a release worker binary, which this session
   did not build. Reason: same budget.
3. **Mutation survivors needing a guard or a written classification** (each needs
   a guard this session has SEEN fail, so each is a test + a run, not a line):
   WAttach A1, A2; WCapture CC1, CC2, CC4, CC7, CC8; WAgentsDetect M10, M12.
   WDoorHttp D1b is classified above as equivalent-and-masked and needs no guard.
4. **Header audit, 13 files** (list in `e565f760`'s body). Nine are v3-new
   modules whose header has to SAY they have no v2 counterpart, which is a
   judgement per file; `session/respawn.rs` has no `//!` header at all.
5. **Cross-track gaps to report, not worker-fixable:** the worker Connect client
   sends no worker credential; the v3 coordinator does not fill
   `recovery_metadata` (v2 returns it only to a worker principal).

## Build rule (host-wide, user-agreed)

Every cargo command: `export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/mike/repos/roost-v3-worker/target-track`,
run as `flock <worktree>/target-track/.roost-build.lock /home/mike/repos/roost-build-slot cargo …`.
Never set RUSTFLAGS. `/home/mike/repos/roost-target-sweep <worktree>/target-track`
after each gate. Never end a turn while waiting on a build: block in the
foreground.

## Earlier sections (kept for the arm table and the decisions)

The downstream arm table, the deliberate-deviation list and the wave-1
per-slice state are in git history at `0f79ffae` and `28e3cd1d`.
