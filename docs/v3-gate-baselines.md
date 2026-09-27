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
