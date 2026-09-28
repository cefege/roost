# Coord lead handoff — C5 COMPLETE, tree clean and pushed

**This note is a MOMENT, not a state. Read `git status --porcelain` and
`git log --oneline` in the worktree before trusting any line below.** Two other
agents wrote into `/home/almalinux/repos/roost-v3-coord` during C5 and several
commits below are THEIRS, adopted as preservation commits rather than re-derived.

## Build environment — the only correct one

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
       CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-coord/target-track
```

One compiler at a time per target dir. Slices edit and report; the lead compiles
and routes diagnostics by file.

## MEASURED — and each figure carries the mechanism that bounded it

| Claim | Figure | Bounded by |
|---|---|---|
| `cargo check -p roost-coord --all-targets --keep-going` | **0 errors**, 1m07s, 30 files | nothing — `--keep-going` is a total |
| `cargo test -p roost-coord --no-fail-fast`, **run 1** on `26782410` | **618 passed / 1 failed / 3 ignored, 97 binaries** (96 `ok`, 1 `FAILED`) | one uncontended run on a committed tree |
| `cargo clippy -p roost-coord --all-targets -- -D warnings` on `887f1d97` | **exit 0**, `Finished` 2m06s | **nothing** — it reached every target and emitted nothing, so it is a TOTAL, not a floor |
| `cargo fmt` | run by another agent (`7c3b90d3`) | — |
| `cargo xtask lint` | **NOT RUN** — `xtask/` is integrator-owned | no `checked N inputs` line to quote |

**Run 2 was never observed.** Three attempts, each blocked by a concurrent agent's in-flight edit rather than by a defect in C5 work: the target dir was cleaned externally mid-run; then `sessions/workspaces.rs:17` failed E0432 on a missing `mod` line; then the same line failed E0364 twice, re-exporting `pub(crate)` items as `pub`. Each was committed as a preservation commit with the failure named. **So the C5 gate is one measured run plus a clean clippy, not two agreeing runs. That is the honest state and it is the one thing left to close.**

The single failure in run 1 is `mcp_relays_authority::a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout` — port answers `Internal` where v2 answers `Unavailable`, changes client retry behaviour. **NOT MINE TO DECIDE.** Not fixed, not hidden, not ignored away.

## C5 commits (mine, in order)

| sha | what |
|---|---|
| `26782410` | the flush turn charges the ACK window — six red tests, one root cause |
| `f351e586` | one `AgentStatusOrder`; the two fixtures that stood in for the product; contract §11 fixture rule (**B/C/D written by CoordLeadC2 before this lead took the phase, credited in the body**) |
| `e7dae9d4` | merge `origin/v3` |
| `d1728b32` | four clippy defects: `too_many_arguments` (8 args / 34 sites → `ViewShape`), a useless `format!`, a `fold`, a no-op `as_ref` |
| `1c2a6a2d` | the wait-bound test must not short-circuit |
| `b175da44` | a doubled comma I generated |
| `887f1d97` | an identity map (another agent's, adopted) |

## The six C5 failures and what actually caused them

| Test | Cause |
|---|---|
| `sync_feed_adapters` ×2 | **one defect.** `AckWindow::record_sent` was the only writer of `last_sent_seq` and nothing in `src/` called it, so `next_sequence()` never advanced and every frame carried `delivery_seq == 1` — which is why a cell stayed fenced and the close frame came back numbered 1 |
| `sync_feed_volatile:126` | name read inverted against its own body; split into an applicability gate + v2's own-echo drop |
| `middleware_security_headers:174` | the test called `build_csp` directly with a hand-built list — the wrong layer. `build_csp` unchanged: v2 has two callers and both derive the twin |
| `agent_status_ordering:334` | **test defect** — expectation in fixture-index order, by a fixture built not to be in that order |

## Clippy: nine defects, none visible to check or test

7 `expect_used` errors in two shared fixtures; `too_many_arguments`; `useless use of format!`; `unnecessary_fold`; `map_all_any_identity`; `useless_asref`; a doubled comma. **The `map_all_any_identity` is the one worth keeping:** clippy said `.all()` was more succinct than a `fold`, I applied it, and the next run disagreed — because `.all()` short-circuits, and that test registers sessions until a bound refuses one, so short-circuiting stops at the first refusal and **the bound is never reached**. The test would have passed without exercising the property it is named for. A linter's suggested fix was a defect, caught only because a second gate run was still going to happen.

## The allow rule, settled by measurement — and my measurement FALSIFIED the prediction

The integrator's rule was "a test FILE carries the allow iff it has a helper site". My exit-0 clippy disproved it for coord: `sync_feed_support` and `workspaces_support` have helper sites, declare no allow, and pass clean. **The corrected rule, which is the integrator's and which I verified independently by listing every fixture's consumers:**

> **A COMPILATION UNIT carries the allow if any file in it has an expect/unwrap outside a `#[test]` body.** For a test binary that is the root; for a shared fixture, every consumer declares it or the fixture declares itself. The only failing combination is a fixture with a consumer that does NOT — and that is invisible from the fixture's own file.

Run over every coord fixture by consumer: **zero gaps.** That independently explains the exit 0. It also retires the "62 sites" figure — that counted occurrences in files, where the deciding question was never about files.

## MUTATION ROWS — all UNRUN, none is coverage

Sent as text to the integrator for `docs/v3-wave-gate.md`. Harness at `/tmp/coord-mutate.sh`, outside the worktree, inert until invoked: backs up outside the tree, refuses to mutate if the backup does not match, verifies restore by sha256 in a trap, reports a compile failure as INCONCLUSIVE.

`C5-1` egress charge reverted → `sync_feed_adapters` `:137` AND `:81` fail; the binary's other four still pass (they never reach a flush, which is what proves the row isolates the charge). `C5-2` delete the `announced.contains` arm in `send_queue.rs` → `:148` fails, `C5-1`'s test still passes. `C5-3` **PRE-REGISTERED AS NOT EXPECTED TO BITE** (tests assert `sendable.delivery_seq`, never the envelope field) — recorded so its silence is never read as coverage. `C5-4` delete `tables.active.remove(...)` at `status_hub.rs` **`:272`, not `:273`** → the ordering test and the delete test fail. `C5-5..8` the four presence rows. `C5-9` delete `security.rs:104` `websocket_twin` push → the CSP test at `:181` fails.

**`M1-MOUNTED` / `M1-PREFLIGHT` — RE-REGISTRATION, not re-pointing.** Their names appear in `1a384d7c`'s body as NEVER RUN but the row text was never written into the repository. FSec's `build_csp` row is **dead text** — no test calls `build_csp` directly any more.

---

# CARRIED — owners named, with `file:line`

**C2 → CoordLeadC2 via WL1.** `src/http/upgrade.rs:97-109`: `UpgradeDecision::Admitted { .. }` returns **401**. The comment above it asserts the arm is unreachable; **it is the success path.** Route mounted at `listener.rs:197`, six-step machine complete. Also `upgrade.rs:177-185` and `:141-144`: `SyncUpgradeDecision::Admitted` returns 401 unconditionally and the admission is fed `tab/since/flow/sync_v` all hard-coded `None`, so contract §8.1's query contract is never read. **No worker and no browser can connect to this coordinator**, and no test can see it because the arm that returns 401 has no test asserting a connection.

**C2 → CoordLeadC2 via WL1.** 661 lines of contract §7.3/§7.5 unreachable — `announced_barrier.rs` (330), `announced_types.rs` (239), `rate_window.rs` (92). `grep 'worker_link::' src/` outside `worker_link/` returns exactly one module, `upgrade_admission`. Unlike `feed/` this substrate is **tested and green** — correct, specified, tested, unreachable. Method: one path-based check, NOT the full method; **no privatise check run.**

**C3 → CoordLeadC2 via SY2.** The firehose has no engine: `sync_ws/mod.rs:26-41` declares **sixteen** modules; `socket` and `driver` are not among them and **neither file exists**, while `feed/mod.rs:19-24` names them as the future owner. Immune to all three search artefacts. Path-based corroboration: `sync_ws::feed` outside its directory has exactly one importer, `services.rs:47` — the composition root, **and being in the composition root is not reachability.** Documented deferral, unlike `worker_link`'s 661 undocumented lines. SY2 must satisfy all 13 `BUS_FRAME_ADAPTERS` rows **and** reconcile the six the table omits. **ORDERING: the driver and a working `/ws/coord-sync` upgrade are ONE piece of work — a bus publish reaching a socket's viewport cannot be demonstrated while `Admitted` answers 401, so C3's new-behaviour check is unrunnable until the upgrade is fixed.**

**C3 → CoordLeadC3 (me).** `roost_coord::auth::bootstrap_tokens::mint_host_bootstrap_token(database, kind, label, now_ms)` — no TTL parameter, `now_ms` a clock seam not a policy seam. Owed to CliLeadL. Bounds Phase 6: `AuthMintBootstrap` (mine) + `AuthRedeemWorker` (WorkerLeadW's) means a fresh host cannot be demonstrated end to end.

# C3 scoping — the 31 `AwaitingDomainPort` rows map to the 11 slices

Workers: `WorkersDeployStart/Output`. Sessions: `Spawn, List, Attach, Kill, Rename, Input, CursorPos, AssignWorkspace, GrantLocalTerminal, NegotiateLocalTerminalPeer, SearchGlobal, CancelGlobalSearch, Prompt`. Attachments: `GrantDirect, DirectStatus, NegotiateAttachmentPeer, AttachmentProbe, AttachFileChunk, DeleteAttachment, FilesRead, FilesReadChunk, FilesListDir, FilesMkdir`. Diagnostics: `AuditList, MiscMetrics, DiagSnapshot, DiagDebugLogBatch`. `AuthCoordIdentity`. **The ledger column is not the authority** — the authority is whether the arm in `service_impl.rs` calls a `handle_*` or a reply builder. 48 `delegated_*` arms against 53 that call a real handler.

# NOT AUDITED — the map with the holes marked

Nobody has asked a reachability question about: `sync_ws/terminal/**` beyond the snapshot seams, `terminal_view/**` beyond `registry.rs`, `push/**` beyond the sender fence, `maintenance/**`, `middleware/rate_limit.rs` beyond its refusals, `db.rs` beyond open/migrate, and `rpc/service_impl.rs` beyond the 48 delegated arms. **On this week's evidence that list is where the next defect is.**

---

# C5 STAGE 2 — measured under CoordLeadC3, every number re-read

**The act, then the value.** `git status --porcelain` and
`git log --oneline -6` first, always. This section is a moment; the worktree
is the state.

## What is DONE and pushed (`origin/v3-coord` 0 ahead / 0 behind at the read)

- **Clippy clean (run 12), and it is a TOTAL, not a floor.**
  `cargo clippy -p roost-coord --all-targets -- -D warnings` → **exit 0** over
  lib + all 95 test targets. The target list is *exactly* that: no `[[bin]]`,
  no examples, no explicit `[[test]]` in `crates/roost-coord/Cargo.toml`, and
  no `src/bin` or `examples/`. **Twelve runs to get here.**
- **The one agent-status pin is GREEN against the shared copy** —
  `cargo test -p roost-coord --test agent_status_rpc` → **9 passed / 0 failed**,
  `the_list_answers_in_session_id_order_with_derived_prompt_that_asked_about`
  included. **This is the first run ever over the shared
 `roost_protocol::wire::agent_status::AgentStatusOrder`**, because every prior
  run predated both the `v3` merge and the deletion of the coord-local copy.
- **All six over-cap files split**, every resulting file under 400,
  `cargo fmt -p roost-coord` clean, and `cargo check -p roost-coord
  --all-targets --keep-going` → **exit 0**.
- **`build_csp` privatised** at `src/middleware/security.rs:146`, and the clean
  build is the proof rather than a grep.

## The four errors the splits cost, because a move is not a move

`cargo fmt` and a line count saw none of them; `cargo check` saw all four.

1. `workspaces.rs` re-exported `detach_members`/`unclaim` with `pub use` while
   both are `pub(crate)` in the new file — **E0364**. A re-export cannot widen
   visibility; the split made the difference between the two spellings mean
   something for the first time.
2. `mints.rs` imported `Caller` from `auth::principal`, which has no such name
   — **E0432**. It is `coord_core::Caller`. I copied the path from the fixture's
   own import list and took the neighbouring line.
3. My own repair of (2) then **overwrote** `use super::{anonymous,
   pubkey_b64};` with a blank line — **four E0425s**. A `PUT` that replaces a
   line in order to insert *after* it deletes the line it was meant to keep.
4. Both split parents kept imports the move had stranded — four unused in
   `auth_device_support/mod.rs`, all four `rsa` imports in
   `cf_access_keyring.rs`.

**The standing lesson: a cross-module move has three parts — the new file, the
cut, and the declaration — and the imports each half stops using.** The first
two were already in this track's history as a commit that shipped a
non-compiling tree under a "PRESERVATION COMMIT" body.

## The split that proves itself

`agent_status_rpc` reports its wait tests as `waits::a_wait_…`, so the
`#[path]` submodule kept **9 of 9** tests. **A split that silently dropped a
test would still print a `test result:` line** — the count is the only thing
that distinguishes "split" from "lost", so read the count, not the exit code.

## STILL OUTSTANDING, honestly labelled

- **Run 2 of the full `--no-fail-fast` suite** has not been taken on this tree.
  Run 1 (618/1/3) predates the `ViewShape` repair, all six splits and the
  `build_csp` change, so it is not evidence about the current tip.
- **`cargo xtask lint`** has not been run in this session; the over-cap work
  was driven by a line count, not by the gate, so the gate is still owed.
- **Clippy has not been re-run since the splits and the privatisation.** The
  exit 0 is real but it measured the tree *before* them; the brief's own
  reasoning — clippy may change the tree, so a run against a moved tree
  measures the wrong thing — applies symmetrically.
- **The mutation rows** were run in one announced window through
  `/tmp/coord-mutate.sh`; verdicts are in the gate report to Main, and
  `/tmp/coord-rows.log` is the raw evidence.

## Re-measure before quoting any of the counts below

`AwaitingDomainPort` **32** in `src/`; `delegated_*` **49** in
`rpc/service_impl.rs`; `#[ignore]` **3** in `tests/`
(`push_sender_bounds.rs:175`, `sync_v2_send_queue.rs:215`, `:257`). Older notes
say 31 / 48 and are **stale by one each** — they predate the `v3` merges.
