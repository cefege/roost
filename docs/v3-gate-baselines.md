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
