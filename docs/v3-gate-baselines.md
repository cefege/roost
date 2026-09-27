# v3 gate baselines

Every phase gate in the Rust rewrite runs the same Playwright suite that
already guards v2. A gate that only says "all tests pass" cannot tell a Rust
regression from a suite that was already red, so each tier records what the
**all-TypeScript** stack does on the same machine, and a later gate is read
against that number.

**The rule this file exists to enforce:** a spec "passes at this gate" only if
it passes in the baseline below. A spec that skips in the baseline must skip
for the same reason; a spec that is newly skipped is a gate failure dressed
up as a non-event.

## Baseline: the all-TS stack

`bun run test:terminal` on `main` (`96f4db25`), Linux x86_64, 8 cores, run
while the four Rust tracks were compiling. One correctness pass at Playwright's
own worker sizing, then one serial perf pass — the same two passes
`bun run test:terminal` performs, unmodified.

| Pass | Collected | Passed | Skipped | Failed | Wall |
|---|---|---|---|---|---|
| correctness | 145 | 142 | 3 | **0** | 12.3m (4 workers) |
| perf (`@serial`) | 18 | 15 | 3 | **0** | 45.7m (1 worker) |
| total | 163 | 157 | 6 | **0** | 58.0m |

**The baseline is fully green.** The rewrite plan expected
`terminal-peer.spec.ts:61` ("loopback wins before a WebRTC peer is allocated
and keeps Sync metadata live") to fail; it passed in 20.1s. Every later gate
is therefore held to the full 157, with zero tolerance.

### The six skips, and why each is not a Rust gap

Every one is a self-skip inside the spec, keyed to a capability this Linux
box does not have. Each must keep skipping for the same reason; a Rust gate
that makes one of them *run* is a change in platform capability, not a fix.

| Spec | Why it skips here |
|---|---|
| `composer-mobile-input.spec.ts:86` | mobile composer preserves and submits terminal input — mobile viewport capability |
| `terminal-mobile-font-width.spec.ts:260` | WebKit iPhone text inflation — macOS-only WebKit engine, per `playwright.config.ts` |
| `voice-dictation.spec.ts:25` | desktop passive drafting preserves a selected reader — needs the dictation/mic path this box does not expose |
| `perf.spec.ts:282` | trusted key, shallow/deep reveal, child-observed resize |
| `perf.spec.ts:286` | optimistic first marker paints while spawn response is held |
| `terminal-switch-perf.spec.ts:94` | activating a pane that moved while inactive costs one complete baseline |

The last three are perf cases gated on the same precondition as their
sibling cases in those files, which do run.

## Deviation: one spec did not reproduce on `v3`, and the baseline is NOT restated

`bun run test:terminal` on `v3` @ `9b9611f1` (the TS stack, `ROOST_SMOKE_WEB_DIST`
unset) returned **141 passed / 1 failed / 3 skipped** in the correctness pass,
against this baseline's **142 / 0 / 3**.

The failure: `[firefox-peer] › smoke/terminal/terminal-peer.spec.ts:263` —
*"invalid offers, unavailable grants, expired grants, and identity mismatches
fall back without recreating the PTY"*, `@serial`. The stack log line for that
run reads `smoke stack: coordinator=typescript worker=typescript
web=apps/web/dist`, which is the default the knob is required to preserve.

**Reproduced twice, in isolation, and it passed both times**: 1 passed in 45.1s
and 1 passed in 43.0s, same project, same `-g` filter, `--reporter=line`.

**That is not a finding, and the baseline is not being restated.** What the
isolation runs establish is narrow: the spec is not reproducible in isolation.
What they do **not** establish is that it is a flake, and the difference matters
because a claim of "flake" is a claim about cause that nobody has evidence for
yet. The full run put four Playwright workers plus four Rust tracks' cargo on
eight cores — load average 25 to 30 during the run, against roughly 8 for the
isolation runs. A WebRTC offer/grant timeout is exactly the shape that load
breaks and that recovers on its own, so load is a plausible cause and not a
measured one.

**The rule for reading this later.** A later gate is held to the full 157 with
zero tolerance, and this deviation is **unresolved, not forgiven**. The next
full `test:terminal` on this tree must either come back 142/0/3 — which closes
it as non-reproducing under load — or reproduce it, which makes it a real
failure to diagnose. Do not read the two green isolation runs as a cleared
gate; read them as the only evidence that exists, and note what it does not
cover.

## The integrator tree's own gate, after the bootstrap commits

`v3` @ `1c9579e8`, Linux x86_64, 8 cores, run while four track worktrees were
compiling in their own target directories. This is the number the integrator
publishes, and it is the only figure in this file that measures the Rust tree.

| Gate | Result |
|---|---|
| `cargo test --workspace --no-fail-fast` | **1437 passed / 0 failed / 0 ignored** across 173 binaries |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean**, confirmed on two independent runs |
| `cargo xtask lint` | **0 violations**, `checked 880 inputs`; 35 `xtask` tests pass |
| `cargo xtask fmt` | formatted, working tree unchanged |
| `bun x tsgo -p tsconfig.base.json --noEmit` | **clean** |

**Up from the 1420 the plan recorded at `a464fa3e`**, and the difference is real
work rather than drift: the `AgentStatusOrder` ordering module and its tests, the
`lint_table` rule, and the `xtask` self-tests that came with the DAG and design
changes.

**Two things this number is not.** It does not cover the four track branches —
each is mid-wave, and merging any of them turns it red for reasons that are
progress rather than defects. And it was measured **before** `roost-keeper`'s
lint-table copy lands, which puts that crate under four lint groups it has never
faced. The carry list predicts that merge turns the workspace clippy red, so **a
clean workspace clippy on `v3` is a statement about `v3` and not about the
programme.**

## Where each track stood after it took `v3`, 2026-09-27

Every track branch was merged to `v3` on 2026-09-27, and each was then
measured with `cargo check --workspace --all-targets` in its own worktree. This
is a COMPILE count, not a test count: it is the lower bound a track lead starts
from, recorded so a later track number can be read as progress rather than as
a fresh surprise.

| Track | Tree | `cargo check --workspace --all-targets` |
|---|---|---|
| CLI | `v3-cli` @ `7fbea312` | **0 errors** |
| Worker | `v3-worker` @ `8a85f523` | **0 errors** |
| Web | `v3-web` @ `13ce9833` | **51 errors**, all in `roost-web-terminal` (lib and lib test) |
| Coordinator | `v3-coord` @ `3b043a6b` | not re-measured; its work is already merged into `v3` |

**The web number is a real defect and it was invisible to CI.** `roost-web-terminal`
fails with the SAME 51 errors on `wasm32-unknown-unknown` as on the host, so it
is not a `cfg`-gating gap — it is a half-finished refactor. The renderer was
being split into sibling `impl` files (`cell_renderer/{scrollback,eviction,
history_page}.rs`) and those modules call `CellGridRenderer` methods and types
that do not exist on the parent; `web-sys` features for `Element::{children,
style,class_list}` are missing from the manifest. It went unnoticed because
`ci.yml`'s wasm job builds neither `roost-web` nor `roost-web-terminal`.

**The `kill(-1)` test, run before any `cargo test -p roost-cli`:** commit
`56a6bb59` is an ancestor of every track branch, so
`crates/roost-cli/src/dev/signal.rs` is the fixed version everywhere. The
dangerous version of that file exists on no branch in this family.

## Track gates as they land

### CLI: `v3-cli` @ `b35f0f65`, merged into `v3` as `0ad703be`

`cargo test -p roost-cli --no-fail-fast`, **two agreeing runs**: all 37
`test result:` lines read `ok`, 0 failed anywhere, both times. The baseline on
this branch was **17 failures across 11 binaries**.

The 17 were **6 product defects and 11 wrong expectations**, and the split held
under re-reading. It is worth recording WHY, because "17 red" reads as a broken
port and it was not: the product defects were a payload preview that trimmed
where v2 collapsed in place; a dial-URL resolution that asked
installed-then-ambient *per name* so a shell exporting the most specific name
outranked the installed definition and silently enrolled the next machine at the
wrong door; a service spec that took `GIT_SHA` from the running binary's
compile-time stamp, so every definition named a commit nobody installed; a
failed commit decision exiting 1 instead of 8, which a wrapper reads as
"retry the same transaction"; and — the one that would have broken the cutover —
booleans written with Rust's `Display` (`ROOST_TRUST_PROXY=true`) where the
loader reads `== "1"` and `parse_terminal_peer_enabled` refuses `true`
outright, so installs came up with the wrong front-door policy or would not
boot at all.

That last one had a second defect behind it, found by following the failure one
assertion further rather than stopping at the fix: the Cloudflare Access pair
was written BLANK when unset, and the host refused a blank team domain while
`normalize_https_origin` reads a blank public URL as unset. **Every coordinator
installed without Cloudflare Access got a definition it could not boot from.**
The root cause was in the host loader and is fixed there; the CLI omission is
kept as v2 parity. A test that stops at the assertion it was given will find
the surface, not the cause.

`clippy -D warnings` and `xtask lint` were **not run** on that branch — the
build queue was spent getting the push out. Six pre-existing `roost-cli`
warnings are still owed there, and `xtask lint` has never been run in its
workspace form by anyone.

### Worker: first-ever total, `v3-worker` @ `8a85f523`

`cargo test -p roost-worker -p roost-keeper -p roost-term --no-fail-fast`, **one
run: 530 passed / 48 failed / 0 ignored, 83 binaries.** No worker total had ever
been recorded before this. **This is a triage baseline and NOT a gate figure** —
one run is not two, and the tree moved substantially afterwards.

The finding that matters is the composition of the 48: they collapse to a handful
of root causes, and **exactly ONE is a product defect**. A reviewer who reads
"48 failing" and concludes "this port is broken" would be wrong.

**The first decomposition published here was an UNDERCOUNT, and the correction is
the useful part.** It said seventeen failures were one fixture constant and nine
were one fixture shape. The real extent is **sixteen call sites** of the non-UUID
cell stream id — not four — across `session_cell_sink.rs`, `session_cell_emit.rs`
and `session_raw_metadata.rs`, so `next_cell_frame` returned `Unbuildable` and
every later assertion saw `Withheld(BaselineOwed)`; and **every `WorkerFp` literal
in the tree is 63 hex characters rather than 64**, across four files including the
17-test `session_support` fixture. Three binaries also hit an unset `HOME`
independently, which is a property of `WorkerBoot::resolve` and not of any one
fixture. The total was right; the explanation of it was not. A published
decomposition that turns out to undercount is worse than none — it sends the
next reader hunting one bug where there were sixteen.

The lesson generalises past this number: **triage a count to its root causes,
then verify the extent of each cause before you publish it.** A decomposition is a
claim about every row in the count, and it is as wrong as a count is if it
undercounts one of them.

The real one is worth its own paragraph. `host/install.rs`'s `closing_quote`
returned the index PAST the closing quote, so `split_once('=')` produced a
variable name with a leading `"` that matched no key, and **every systemd scrub
took the "nothing on this line is the key" branch and reported `removed: false`**.
A redeemed bootstrap token survived in the unit; a spent force-live-retire
authorisation survived to re-authorise killing every PTY on the next restart.
Twenty-nine lines of span arithmetic, and a credential and an authorisation both
failing open.

`roost-keeper` on the same branch: `cargo test -p roost-keeper --no-fail-fast`,
**two agreeing runs, 23 binaries, 0 failed.**

### Keeper client: the plan's premise was stale

The plan recorded three open keeper-client defects on `v3-worker`
(`wait_for_reply` discarding non-matching frames, six missing client frames,
`resize()` discarding the applied seq and geometry). **All three were already
fixed** by commits `94bab2b7` and `be68afa0`, merged in at `8a85f523`, with 14
tests covering them. The plan was reading a doc line the branch had moved past.
The deferral was proved with a mutation rather than an assertion: reverting
`client_frames.rs:121` to the pre-fix drop gives **0 passed / 2 failed**, both
`left: []`.

The lesson is the one this file exists to enforce. A defect list in a plan is a
hypothesis about a tree; a measurement is a fact about one, and the two drift
apart at exactly the rate the tree moves.


## How to read a later gate

- **Phase 2** (Rust worker, TS coord): no spec that passed in the baseline
  may fail. `terminal-delivery.spec.ts:15` ("browser smoke flow creates and
  cleans its resources") is the load-bearing one and must pass.
- **Phase 3** (Rust coord, then both): same rule, with
  `ROOST_SMOKE_COORD_EXECUTABLE` set alone first, then with both.
- **Phase 4** is not Playwright — it is
  `crates/roost-client-core/tests/headless_client.rs`, an in-process Rust
  coord + worker that must paint a `MARKER` into a replica viewport.
- **Phase 5** (all-Rust): the full 157 on Chromium and Firefox, plus a
  production build with the `smoke` feature off containing zero occurrences
  of `__smoke` in the bundle.

A gate that cannot run the suite at all has proved nothing. Say so rather
than reporting a partial pass as a green one.

### Two more, both of which cost a whole agent-hour to learn

**A diagnosis that survives only the fix it proposed is not a diagnosis.** A
worker test failed on a 5 s `SpawnNotAcknowledged`, which reads exactly like the
load story above, so the explanation was load. It died on a **serialised** re-run
— same result, no other track building — and what was underneath was
`KeeperFixture::start`: a strict accept/serve loop that serves exactly ONE
connection at a time. A test holding the first pool alive across a second
connect makes that second `connect` retry to the timeout, and the failure it
produces **names a SPAWN rather than a connect**. So the rig lied about which
seam it broke, and the plausible cause pointed at the product.

The rule is not "be more careful". It is: **if your explanation is load, and the
re-run under quiet conditions reproduces it, your explanation was wrong.** Load
is the cheapest available explanation and it is the one most likely to be
assumed rather than eliminated.

**A test red for a reason unrelated to what it tests trains a reader to ignore a
red in that file.** Two of this track's reds were literal-versus-assertion, not
logic: one asserted 24 for a 25-byte literal, another named a spawn when the
rig had broken a connect. Neither is expensive to fix, and both are expensive to
leave, because the cost is not the wrong assertion — it is that the file stops
being read. **When a test fails, check what the failure is actually about before
fixing what it appears to be about.**

**And the one that is a process rule rather than a testing rule: a shared
working directory is not yours.** `git add -A` across a tree three agents were
editing captured a sibling's uncommitted fix in its pre-fix state and silently
reverted their work; neither noticed until a test failed, and the fix existed in
exactly one place for as long as that took. Stage an **explicit path list** and
ping the owner before committing anything you did not write. `-A` cannot
distinguish "I read this and it is right" from "this was on disk", and on a
shared tree the second is most of it. An unstaged file is a five-second fix; a
wrong commit is a red tree for everyone.


### Two ways to misread a red run before you have read it

**A `SpawnNotAcknowledged { timeout: 5s }` under load is LOAD, not a refused
spawn.** `SPAWN_ACK_TIMEOUT` is 5 s and it is not a fixture's to widen. A test
binary that opens REAL PTYs competes with every other thing compiling on the
machine, and the first observed instance came from a real keeper starved by
three concurrent track builds. At a gate, that failure is evidence about the
machine. Reading it as a product-seam defect sends you to debug a keeper that
was never asked a question it could not answer, and the expensive part is the
hours, not the mistake. **If a spawn timeout is the only red, re-run it with
the machine otherwise idle before believing it.**

This is the same shape as the known `terminal-peer.spec.ts:263` deviation: a
spec that passes isolated and fails under full load is a load signal until a
quiet full run says otherwise.

**A compile error in one crate is a COMPILE error, not a verdict on the port.**
The worker track's first recorded total was 48 failures across 83 binaries, and
it would have been easy to read that as a broken port. Twenty-nine lines of
span arithmetic in a systemd scrub were the only product defect in it; the
other 47 were seven fixture-shape problems — a `WorkerFp` that wants 64 hex
where the fixture passed a UUID, a cell stream id that must be a UUID, an unset
`HOME`. **Triage a red count to its root causes before you characterise it.**
A count and a character are different claims, and only one of them is
supported by the number.

**A test that has never RUN is a liability, and a red one is information.** Five
named tests in the worker track were written and never executed, blocked behind
a `String`/`&str` in a fixture. Reporting them as "the slice is written" would
have been a claim about bytes rather than about behaviour, and the gate would
have inherited four of them as unverified. When a figure is unrun, say
`unrun` — an unrun gate item labelled unrun costs nothing, and a claimed one
costs the gate.


## Perf numbers worth not regressing

From the baseline's own instrumentation, since the perf specs assert against
their own budgets and a silent 2× drift is easy to miss in a pass count:

| Case | Baseline |
|---|---|
| `history_scroll_plan` | 8 steps, 250-row pager fetch, 500-row ahead |
| `history_scroll_latency` | median 178ms, max 293ms per step, 4 total RPCs, 30s budget |
| navigation → first paint | `cold_driver_to_paint_ms` 914, fresh navigation 3.0–3.7s |
| 20k flood | 306ms wall, 0 dropped phases, 19,970 retained, 259 DOM nodes, 32 cell rows |
| bounded deck (16 spawned) | 9 mounted slots, reveal p50 47ms, p95 57ms |
| peer perf (Sync vs loopback vs direct) | asserted by `terminal-peer-perf.spec.ts:7` |

Retained-marker bounds are worth keeping in view because they are the
history-corruption tripwire: a Rust renderer that drops the retained floor
will pass every functional spec and still lose scrollback.
