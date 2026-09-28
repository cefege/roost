# Track U slice context (shared by every web slice agent)

## Goal
Roost v3 (Rust) reaches full behavioural parity with v2 (TypeScript, under `apps/` in the same checkout). You port one slice of v2's web client (`apps/web/src/**`) into the Rust crates `crates/roost-client-core` (framework-free state machine + wire codecs), `crates/roost-web-terminal` (imperative web-sys renderer, no framework) and `crates/roost-web` (Dioxus 0.7 app). v2 behaviour wins wherever port and v2 disagree.

## Worktree and ownership
- Worktree: `/home/almalinux/repos/roost-v3-web` (branch `v3-web`). Absolute paths under it ONLY. Never touch `/home/almalinux/repos/roost` or any other worktree. Never stop/kill/restart any process you did not start (v2 services, keeper pid 2325919).
- You own ONLY the files your task lists. Other agents are editing other files in this same tree concurrently. If you need a one-line registration in a shared file (`mod.rs`, `lib.rs`, `components/mod.rs`, `app.rs` route arm, `Cargo.toml` web-sys feature list), make exactly that minimal edit, re-reading the file first (the edit tool rejects stale tags — re-read and retry), and list it in your report.
- NEVER commit, NEVER run `cargo fmt`/`cargo xtask fmt`, NEVER run `--update-*-baseline`, NEVER run git commands that change state (stash/reset/checkout/commit). Read-only git (diff/status/log) is fine.

## Build rule (mandatory, host-wide disk budget)
Every cargo or dx command:
```
source /tmp/webenv.sh && c <cargo args>      # e.g. c check -p roost-web --all-targets
```
`/tmp/webenv.sh` sets `PATH`, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-web/target-track`, and `c` wraps `flock <worktree>/target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …` with the disk check. Do not set RUSTFLAGS. Builds serialize on the lock — compile deliberately (write a coherent batch, then check), not after every line. Use `--message-format short` and grep errors to keep output small. Prefer `c check -p <crate> --all-targets` and `c test -p <crate> --test <name>` (single target) over whole-crate runs. For wasm-gated code also run `c check -p <crate> --target wasm32-unknown-unknown`.

## Rules (non-negotiable)
- Every `.rs` file ≤ 400 lines counting ALL lines (tests included). Split into submodules before you hit it; move tests to `crates/<crate>/tests/<name>.rs` when needed. Never re-pack lines to fit.

- **NEVER end a turn while waiting on a build.** A turn that ends with no tool call makes the harness force a tool choice the model rejects (API 400), which kills you. Long commands may be auto-backgrounded; wait for them by blocking in the FOREGROUND, e.g. `flock /home/almalinux/repos/roost-v3-web/target-track/.roost-build.lock true` (bash, timeout ≤ 3600) or a python `eval` loop with `time.sleep`. Only end your turn with your final report (`yield`).
- **Never spawn agents.** Helpers are one level deep: the lead spawns slices; slices never spawn agents (no `task` tool use). Never spawn an agent only to wait.
- Never bind fixed ports 4103/4104/4113/4114. Never stop/kill/restart any process you did not start.
- A 3–6 line `//!` header on every non-trivial file: what it owns, who calls it, what it depends on, and the v2 file(s) it ports by path (`apps/web/src/...`). The header naming the v2 path is how the track proves coverage.
- No `todo!()`, `unimplemented!()`, stubs, placeholder returns, empty `pub mod`, `macro_rules!`, top-level mutable state (`static mut`, thread_local mutable globals, lazy globals), no `unwrap`/`expect` outside tests (tests may `#![allow(clippy::unwrap_used, clippy::expect_used)]` at the test-crate root).
- A `tracing` line (`tracing::info!/debug!/warn!(target: "<area>", …)`) at every state transition.
- DOM boundary: web-sys/js-sys calls only inside `#[cfg(target_arch = "wasm32")]` adapter code; ALL logic (state machines, counters, parsing, formatting, geometry) lives in target-independent types with native `#[test]`s.
- Reuse existing seams; grep before writing a helper. `roost_protocol` owns shared wire shapes; `roost-client-core` owns store/state machines; `roost-web` owns components. `roost-web` may depend only on `roost-web-terminal`, `roost-client-core`, `roost-protocol` (xtask/src/crate_dag.rs) — no `roost-proto` in roost-web; protobuf encode/decode lives in client-core.
- Naming: descriptive snake_case; no `handle`/`process`/`run`/`do`/`manage` alone; no Utils/Helpers/Common modules.
- Inline comments explain WHY only; no narrative comments ("ported from", "for now", "phase").
- Tests: port v2's unit tests for your modules (`apps/web/src/**/*.test.ts` or `apps/web/tests/**`) that pin real behaviour; do not port tests that assert source text, wording or incidental defaults. Tests must catch plausible consumer-visible bugs. Deterministic, no clock/network.
- Every new invariant guard must be seen to fail: mutate the product line it guards, run the test, watch it fail, revert. Record each mutation (file:line, what changed, which test failed) in your report.
- "Who consumes this?": every new enum variant/flag/returned value/rendered element needs a production reader. A handler for an event nothing produces is not coverage. If your slice's consumer lives in a later slice, say so explicitly in the report.
- Before porting a module, grep `docs/FAILURE-INDEX.md` for its file name and exported symbols; an entry whose Guard names a v2 test you port needs the equivalent Rust guard — name the entry in your report.
- Classify any red test you encounter by v2 first (product defect → fix product; test defect → fix test citing v2; belongs to a later slice → `#[ignore = "<slice>: <what>"]` with the assert unchanged). Never weaken an assert.

## Web design system (UI slices)
- No raw values in components/CSS you add: reference tokens (`--surface-0..3`, `--text-hi/mid/lo`, `--md-*` roles, `--md-space-1..9`, the `--md-*-size/line/weight` type ramp, `--md-shape-*`, `--md-elev-0..5`) declared in `crates/roost-web/assets/styles/theme-vars.css` and `crates/roost-web/assets/components/Settings/md/tokens.css` (copied from v2). Legacy aliases only in byte-for-byte copied v2 CSS.
- Compose from the md primitives (`crates/roost-web/src/components/md/`, port of `apps/web/src/components/Settings/md/primitives.tsx`) — `Surface`, `StatusDot`, `Sheet`, `Button`, `IconButton`, `Card`, `List`+`ListRow`, `Chip`, `Dialog`, `MetricTile`, `EmptyState` … — never hand-roll a `div` panel/button/status span. Keep v2's class names and `data-testid`s EXACTLY (the Playwright oracle selects on them) and reuse v2's CSS files byte-for-byte under `crates/roost-web/assets/` (same relative path as under `apps/web/src/`), attached via `document::Stylesheet`/`asset!` or the `[web.resource]` list in `crates/roost-web/Dioxus.toml` (lead-owned; ask).
- Dioxus 0.7: `#[component] fn X(props…) -> Element { rsx!{…} }`, `use_signal`, `use_memo`, `use_effect`, `spawn`, `use_context`. The store is `Rc<RefCell<ClientCore>>` in context; a component that renders store state MUST read the pump's `Signal<u64>` revision during render (see `crates/roost-web/src/pump*.rs` once it lands) so it re-renders.

## References
- Plan section: `docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 5" (read your row).
- `docs/v3-handoff/roost-porting-conventions.md`, `docs/v3-handoff/silent-no-ops.md`, `docs/phase4-client-contract.md`, `apps/web/src/smoke/smokeTypes.ts`, `CLAUDE.md` design-system section.

## Report (your final answer, concise)
1. Files written/changed with final line counts; any edit outside your owned set (exact file + line).
2. v2 files ported (path list) and v2 files in your slice NOT ported with the reason.
3. v2 tests ported (count) and the command + result line proving they pass (`test result: ok. N passed; 0 failed`), plus `c check -p <crate> --all-targets` result, plus wasm32 check result if you touched wasm-gated code.
4. Mutations performed (guard → mutation → failing test).
5. Consumers: for each new public type/fn/variant, its production reader or the slice that will add it.
6. Anything left undone with the exact reason. No stubs allowed — if something cannot be done, leave it out and say so.
