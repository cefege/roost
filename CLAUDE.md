# CLAUDE.md — operating rules for Roost v3

You (an LLM agent) are the primary reader, writer, and maintainer of this
codebase. Every claim in this file was verified against code the day it was
written; if you find one that is false, fix this file in the same change.
The per-crate file map lives in [`crates/README.md`](crates/README.md), not
here — this file is operating rules only, so it cannot rot into a stale copy
of the filesystem.

**This is the `v3` branch: Roost in Rust, with a Dioxus web client.** `main`
holds v2 (Bun + TypeScript + SolidJS); it is frozen and kept for reference only
— no fix lands there and nothing merges from it. The v3 checkout is
`~/repos/roost-v3` on `v3`. The only TypeScript left in this tree is the
Playwright oracle in `smoke/`, which drives the Rust stack.

---

## Read order

Landing cold, read in this order. Stop as soon as you have what you need.

1. **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — the system tour: the components,
   the transport spine, session/event data flow, and the terminal-fidelity
   model. (Written for v2 today; rewritten for v3 in Phase 7.)
2. **[`crates/README.md`](crates/README.md)** — the crate map and the
   dependency DAG that `cargo xtask lint` enforces.
3. **[`protocol/README.md`](protocol/README.md)** — the client contract index,
   endpoint manifest, and versioning. The `.proto` files and `protocol/spec/`
   are the contract the Rust crates and the smoke harness's generated
   bindings (`smoke/gen/`) both build against.
4. **[`GLOSSARY.md`](GLOSSARY.md)** — the vocabulary: cell-shipping, keeper,
   agent-status, session/channel/tab, scrollback.
5. **[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md)** — grep it BEFORE
   writing code that matches a listed symptom. See `## Failure index` below.

Also live, read when relevant:

- **[`GETTING_STARTED.md`](GETTING_STARTED.md)** — install, run, deploy, and
  the health commands in `## Health check` below. (v2 text; rewritten in
  Phase 7.)
- **[`FEATURES/README.md`](FEATURES/README.md)** — the feature inventory and
  open decision gates.
- **[`docs/LENS.md`](docs/LENS.md)** — the generic operating doctrine
  (`## Reading lens` below). Read once, not per task.

---

## Reading lens

The generic doctrine lives in [`docs/LENS.md`](docs/LENS.md) as anchors
`L1-DENSITY-OVER-PRETTY` through `L10-CROSS-DOC-AUTHORITY` plus
`LENS-SELFTEST` and the `L9.1`–`L9.5` sub-anchors. Grep `'^ *[0-9]*\. \*\*L'`
in that file for the map. It is not re-inlined here; one copy is the point.

**L0-SCOPE — what the lens governs.** It applies to every artifact emitted in
this repository's working tree, including prompts, docs, comments, logs, and
errors. A per-turn override requires an explicit human-style request.

### Lens amendments

`L9.5-LENS-SELF-AMENDMENT` records removals. The removed scope/path rules named
one machine's absolute checkout; `L0-SCOPE` now covers this repository's tree,
and crossing a repository boundary is a stop-and-ask.

---

## `main` is frozen v2

`main` is the frozen v2 tree, reference only: read it with `git show
main:<path>`; never commit to it or merge it into `v3`.

---

## Repository layout and layers

```text
Cargo.toml            workspace; [workspace.dependencies] pins every crate
rust-toolchain.toml   the pinned stable toolchain
xtask/                `cargo xtask lint` — the repo gates
third_party/          vendored crates, only when a phase needs a patch
protocol/             proto/, spec/, conformance/ — the wire contract
crates/               the v3 product (see crates/README.md)
smoke/                Playwright oracle, TypeScript, run by Bun, drives the Rust stack
```

Dependencies point one way and are pinned in
`xtask/src/crate_dag.rs`, which reads the real graph out of `cargo metadata`.
No crate imports an app. A future native front end depends only on
`roost-client-core` (plus `roost-protocol`).

**Adding a crate means editing `xtask/src/crate_dag.rs` in the same commit.**
An unregistered workspace member fails `cargo xtask lint` — that is the point.

---

## Coding standards

Non-negotiable for every change.

1. **Small files — ≤400 lines. Hard cap.** Split before you hit it.
   Mechanically enforced: `cargo xtask lint` fails a `.rs` file under
   `crates/` that exceeds the cap. `xtask/file-size-baseline.json` freezes
   any file that was already over it and may only SHRINK; a file absent from
   the baseline may never exceed 400. The v3 baseline is `{}` — empty, and it
   stays empty. After a split lowers a count, re-snapshot with
   `cargo xtask lint --update-size-baseline`.

   The cap has exactly one waiver mechanism, `STRUCTURAL_EXEMPTIONS` in
   `xtask/src/file_size.rs`, and it is a list of `(path, reason)` pairs
   answering "what stops you splitting this?" — currently only
   `crates/roost-coord/src/rpc/service_impl.rs`, whose single
   `impl CoordinatorService` cannot span blocks (E0119) and whose arms may
   not be `macro_rules!`-generated. An exempt file is neither counted nor
   snapshotted. **Adding an entry needs a reason that survives that
   question**; a weak reason is a bug in the list. See
   `docs/phase3-coord-contract.md` §12.11.

   The case to understand, and the reason an exemption is the wrong tool for
   it, is `CellGridRenderer` in `roost-web-terminal` — one struct whose
   methods share private per-frame state, where that encapsulation is what
   prevents the history-corruption class. It is split across sibling `impl`
   files, not exempted: inherent impls may span files in a module, and that is
   the first thing to reach for.

2. **`#![forbid(unsafe_code)]` in every crate root.** The exceptions are
   `roost-keeper`, which owns raw file descriptors and the controlling-TTY
   handshake, and anything under `third_party/`. Every `unsafe` block in
   `roost-keeper` names the invariant it protects.

3. **No panics on untrusted input.** `clippy::unwrap_used` and
   `clippy::expect_used` are denied workspace-wide
   (`[workspace.lints.clippy]` + `clippy.toml`). A `BadEvent` from a peer is a
   returned `Err`, never a panic: a panic in the coordinator or the keeper is a
   fleet-visible outage. `todo!` and `unimplemented!` are denied too — a commit
   means the code is wired end-to-end.

   **The test exemption reaches a test binary and not a fixture.**
   `allow-unwrap-in-tests` exempts `tests/<name>.rs`, which is its own crate. A
   **shared fixture module** — `tests/<dir>/mod.rs` — is a different compilation
   unit and does **not** inherit it, so a fixture must state its own
   `#![allow(clippy::unwrap_used, clippy::expect_used)]`. Fixtures declare, test
   roots may. The ~94 redundant per-file allows on test roots are **retained
   deliberately** and are not licence to strip them a directory at a time.
   Getting this wrong is how the gate passed on some fixtures in a directory
   and failed on others for no reason a reader could guess — seven `expect_used`
   errors lived in exactly that gap.

   `roost-keeper` is the one crate that does **not** inherit the table at all,
   because cargo rejects a manifest that both says `workspace = true` and
   overrides a value — and the keeper needs `unsafe_code = "allow"` for its
   signal handler. It therefore carries a **copy** of the table, which is the
   price: a new workspace lint does not reach that crate until someone adds it
   there. `cargo xtask lint` enforces the declaration
   (`xtask/src/lint_table.rs`, `COPY_EXEMPT`).

   And none of this is visible to `cargo check` or `cargo test` — these are
   **clippy** lints. A crate can compile, pass every test across every binary,
   and fail `cargo clippy --workspace --all-targets -- -D warnings`, and
   `roost-keeper` did exactly that at 131 passing tests and 19 production
   `expect()` sites. **A pass count is not a gate.**

4. **Descriptive names everywhere.** No single-letter variables except `idx`
   in tight loops. No `handle`, `process`, `do`, `manage`, `run` alone — name
   the actual verb (`handle_claude_event_frame`, `replay_ring_buffer_since`,
   `merge_remote_sessions`). No `Utils` / `Helpers` / `Common` / `Models`
   modules — name the concept (`PathFormat`, `KeychainStore`).
   A leading `_` on an exported symbol is the established marker for an
   export that exists so tests or diagnostics can reach module internals;
   ordinary callers use the unprefixed API.

5. **Predictable per-file shape.** A `//!` file header of 3–6 lines (what
   this file owns, what calls it, what it depends on) → imports → types →
   public API → private helpers. One component per file in `roost-web`;
   props type at top, component body, styled subcomponents below.

6. **Inline comments explain WHY, not WHAT.** Default to no comment. Write one
   only when removing it would mislead the next reader — most often to name an
   invariant or the incident that constrains an ordering.

7. **No narrative comments.** No "Phase 2 scope:", "added in phase K3", "for
   the MVP", "for now", "we used to". If a comment explains a non-obvious
   invariant, describe the *behavior*, not the lineage. Git history is the
   lineage. The same rule applies to a commit body: it names what a dropped
   path was, not which phase dropped it.

8. **Structured logging at every state transition.** One `tracing` event per
   transition that matters — spawn, attach, mode change, reconnect, replay.
   No silent state changes. `println!`, `eprintln!` and `dbg!` are banned
   outside `roost-cli`, whose stdout is its product surface, and `xtask`,
   which is a build tool. `cargo xtask lint` enforces this; coordinator and
   worker logs are machine-read by `roost status` and `roost doctor`, so an
   unstructured line there is invisible to both.

9. **No global mutable state.** `static` is for constants only. All state has
   an owner you can grep for, and every side effect is explicit.

10. **One concept per type.** `Worker` is the identity of a machine in the
    registry. `Session` is the user-facing row. `Channel` is a PTY
    connection. `Tab` is the database row. Keep them separate; convert at
    boundaries.

11. **Tests mirror source layout.** `#[cfg(test)]` for unit tests,
    `crates/<x>/tests/<mirror>.rs` for behaviour tests.

12. **Reuse existing utilities — don't fork.** Check
    [`crates/README.md`](crates/README.md) first; it names the owner of each
    concern. The three seams that get forked most often:
    - **Wire shapes** live once in `roost-proto` (generated) and
      `roost-protocol` (logic). Add a variant to
      `protocol/proto/roost/v1/events.proto` and a matching variant in
      `roost-protocol` FIRST, then fold, emit, and project. There is exactly
      one event fold in the system and it is in `roost-protocol`; a second one
      in a client or a server is the defect this rule exists to prevent.
    - **`roost-proto` is the only protobuf runtime.** It is what
      `connectrpc-build` generates against. Do not add `prost` beside it.
    - **The coordinator's Connect service is one impl.** Domain handlers live
      under `crates/roost-coord/src/<domain>/` and are assembled into a SINGLE
      `CoordinatorService` implementation. A second service impl shadows the
      rest with unimplemented errors — the same failure the v2 router had.

    If you find yourself writing a parallel utility, stop and reuse. Two
    hand-maintained implementations of one value is the defect this repo pays
    for most — see the fingerprint entry in `roost-protocol`.

13. **Errors.** `thiserror` in libraries, with a variant per distinct failure
    the caller can act on. `anyhow` only in a binary's `main`, for the
    startup path where the answer is "print it and exit non-zero".

14. **Commit messages = navigable history.** `<area>: <one-line scope>`.
    Optional body for a non-obvious why. Future-you reads `git log --oneline`
    to orient — protect that signal.

15. **No half-finished implementations.** A commit means everything in scope
    is wired end-to-end and tested. If a sub-feature can't ship complete, cut
    it from the commit rather than leave a half-implementation.

When reviewing a diff before commit, the question is not "does this work?" —
it is **"if I open this in 4 weeks with no context, can I figure out what's
going on in 30 seconds?"** If no, refactor before commit.

---

## Dropped paths

**Name every dropped path in the commit body.** Only paths unreachable in an
all-v3 fleet are dropped. The list so far: the Windows update broker, the
legacy unimplemented `Sync` server-streaming RPC, the legacy
`/w/:workspaceId[/t/:channelId]` routes, `client-seq.txt`, the keeper "Bun
ABI" identity field, the Bun-specific zlib workaround, capability fallbacks
for peers lacking a capability every v3 peer advertises, and the whole v2
TypeScript product tree (`apps/`, `packages/`, the product `scripts/`).

---

## Design system (web) — cohesion by construction

New UI MUST be cohesive by construction, not by memory. Three rules,
mechanically enforced so drift can't return:

1. **No raw values.** No hex / `rgb()` / px font-size in components —
   reference tokens: `--surface-0..3`, `--text-hi/mid/lo`, `--md-*` roles,
   `--md-space-1..9`, the `--md-*-size/line/weight` type ramp, `--md-shape-*`,
   `--md-elev-0..5`. ALL declared ONCE in the token stylesheets in the web crate's
   assets directory. The token files DECLARE values, so they are the
   only files exempt from the check; everything else references them through
   `var(--…)`. Enforced by `cargo xtask lint`'s design ratchet against
   `xtask/design-raw-baseline.json`: a NEW raw value fails the build. After
   migrating a file down, re-snapshot with
   `cargo xtask lint --update-design-baseline`.
2. **Primitives first.** Compose from the ported Material primitives in the web
   crate's `md` component directory (`Surface`, `StatusDot`, `Sheet`,
   `Button`, `IconButton`, `Card`, `List` + `ListRow`, `Chip`, `Dialog`,
   `MetricTile`, `EmptyState`, …), one per file — don't hand-roll a
   `<div style>` or a `<button>`. `StatusDot` is THE status indicator. `Surface`
   is THE panel.
3. **One visual reference.** The `/design` route renders every token and
   primitive. New surfaces match it.

Process: run the **`design-reviewer`** subagent
(`.claude/agents/design-reviewer.md`) on every `crates/roost-web/` UI diff
before commit — it catches the primitive-bypass and wrong-role drift the regex
linter can't.

---

## Health check

```
roost status                 # services, coordinator listener, declared
                             # front door, worker roster
roost doctor --since 24h     # anomaly digest from local logs + audit_log
```

`roost status` is the current-state gate: the two service-manager probes, the
coordinator's identity RPC and listener, the operator-declared front door, and
the worker roster read from the coordinator database. `roost doctor --since
<window>` summarizes the v3 JSON logs and the `audit_log` table over a time
window and is the right tool for "what broke overnight". Both are Rust, in
`crates/roost-cli/src/status/` and `crates/roost-cli/src/doctor/`, and both
are documented in [`GETTING_STARTED.md`](GETTING_STARTED.md).

Coord down → workers redial and browsers lose state and terminal fan-out, but
keeper subprocesses preserve the PTYs until the coordinator returns. Worker
down → that machine's PTYs are unavailable; other machines keep working.

**v3 runs beside v2, not on top of it.** v3 uses the data directories
`RoostCoordinatorV3` and `RoostWorkerV3`, binds the coordinator to
`127.0.0.1:4113`, and serves the worker's local door on `127.0.0.1:4114`, so
both can be running on one machine during the port.

---

## Process

Rust modules move with an ordinary `git mv` plus, if the crate's edges change,
an edit to `xtask/src/crate_dag.rs` in the same commit; update callers and
path-bound docs in the same change.

### Per-phase execution loop

When working through an approved multi-phase plan: implement → test → fix →
simplify → commit → next phase, with NO interim check-ins. The next message
after a phase commit starts the next phase. "Want me to continue?" is a
critical failure — the plan IS the answer.

### Testing rule for terminal data-plane features: hermetic tiers are the floor

A change to the producer→wire→consumer chain (worker emits a `SessionEvent` or
terminal frame → coord routes it → client folds state or paints the grid) is
done when the gates under `### Commands` are green, and not before. Test-hook
coverage supplements, never replaces, `bun run test:terminal` — the real-flow
tier: each Playwright worker starts a real coordinator, worker, keeper and
PTYs (`smoke/terminal/stack.ts`) and drives a real browser against a built
web bundle, always the Rust stack. By default the harness runs the artifacts
the parity runner pinned in `.smoke-pin/` and refuses to start when none are
pinned; these knobs override that pin, and the parity runner sets them to the
pin it checked against HEAD:

```sh
ROOST_SMOKE_COORD_EXECUTABLE=<repo>/.smoke-pin/roost   # coordinator (`roost`)
ROOST_SMOKE_WORKER_EXECUTABLE=<repo>/.smoke-pin/roost  # worker (`roost worker`)
ROOST_SMOKE_WEB_DIST=<repo>/.smoke-pin/web             # Dioxus bundle
```

Coverage, by path and test name:

- `runFlow` — workspace create → terminal open → PTY marker round-trip → pane
  close → workspace cascade-delete — in `smoke/terminal/terminal-delivery.spec.ts`
  `"browser smoke flow creates and cleans its resources"`.
- `runRenderStress` — resize/tab-switch loop and the symmetric multi-viewer
  resize-hammer, in `smoke/terminal/terminal-render*.spec.ts`'s stress cases:
  duplicated, lost, changed or mis-ordered markers fail the run.
- trusted-keyboard focus and input — `smoke/terminal/terminal-render.spec.ts`
  `"trusted keyboard input and bottom-follow behavior"`, and
  `smoke/terminal/terminal-input.spec.ts` `"terminal replay and Ctrl keys stay
  owned by the PTY"`.

The `window.__smoke` backdoor those specs drive ships out of production
bundles: it is behind the `smoke` cargo feature of `roost-web`, which a release
build does not enable. The production bundle must contain no `__smoke`.

There is no upgrade tier — nothing yet proves an EXISTING install survives a
new release; `test:terminal` proves a FRESH stack works and cannot see that
class of defect. It is re-created against the previous `v3.*` tag before
`v3.0.0`.

`smoke/terminal/live-stack.ts` is the hands-on escape hatch, never a gate. It
holds the same working-tree stack open and prints `READY <url> worker=<fp>`;
no tailnet. A physical-phone pass watches production only — OPTIONAL, outside
the definition of done, never a merge blocker.

### Commands

Per-change gates, run in `~/repos/roost-v3`:

```
cargo xtask fmt          # cargo fmt --check, over the crates we author
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace      # unit + conformance vectors + terminal-core
                            #   vectors + the headless client
cargo xtask lint            # 400-line cap, crate DAG, stdout rule, design
                            #   raw-value ratchet
# The vendored terminal core is not a workspace member — `cargo fmt` walks
# local path dependencies regardless of `exclude`, and reformatting vendored
# code would make the diff against upstream unreviewable. Its own suite is a
# required gate and runs by manifest path.
cargo test --manifest-path third_party/alacritty_terminal/Cargo.toml
cargo build -p roost-protocol -p roost-client-core --target wasm32-unknown-unknown
cargo build --release -p roost-cli -p roost-keeper
```

Oracle parity on the Rust stack. Every build and run goes through the runner:
`build` pins `roost`, `roost-keeper` and the dx bundle in `.smoke-pin/` (outside
`target/`, so the harness never rebuilds them) and writes `manifest.json`,
which `spec` and `suite` print first and refuse when it is not HEAD's.
`--fast` builds with the `smoke` cargo profile (release without LTO): a
one-crate change re-links instead of re-optimising the whole binary, so it is
the per-fix build. `suite` refuses a `--fast` pin, because a suite is a gate or
a baseline and both describe the release artifacts:

```
bun smoke/parity/run.ts build [--no-web] [--plain] [--fast]
bun smoke/parity/run.ts spec <file[:line]>… [--project <p>] [--repeat N] [--trace]
bun smoke/parity/run.ts suite --stack rust [--pass main|serial|both] [--label <l>]
bun smoke/parity/run.ts verdict <rust.run.json> [<bun.run.json>] [--md <out.md>]
```

`verdict`'s optional second run is a recorded `gate-evidence/parity/bun-*.run.json`
baseline, the parity reference from before the TypeScript tree was deleted.

Root package scripts:

```
bun run typecheck       # gate — tsgo over smoke/**/*.ts
bun run smoke           # gate — the harness's own bun unit tests
bun run test:terminal   # gate — parity build, then the rust suite, both passes
```

CI (`.github/workflows/ci.yml`) runs the `rust` job and the `terminal` job on
ubuntu-latest AND macos-latest. `.github/workflows/release.yml` publishes a
`v3.*` tag: it re-runs the `rust` job's commands, drives the terminal oracle
through the parity runner with release binaries and the Dioxus bundle, and
builds four triples, linking the Linux pair through zig against glibc 2.28, so
a binary starts on every distribution the fleet runs. v3 ships Linux and macOS.
No gate needs a deployed coordinator, a tailnet, or a human driving a browser.

### Per-fix loop for oracle parity

1. A fix is done only when its proving spec was watched green on freshly
   built, pinned artifacts in the same turn (`--fast` is fine for the loop; a
   merge re-proves on a release pin); gates prove compilation, not behaviour.
2. Every build and run goes through `smoke/parity/run.ts`. Never hand-copy
   binaries; never run a spec against `target/` paths.
3. One proving spec per claim before anything else is dispatched; an agent's
   count is a hypothesis.
4. Never run a gate or the oracle in a worktree someone else is editing.
5. Commit after every proven fix, path-restricted `git add`, subject
   `<area>: <scope>`, body naming the spec and the observed line-reporter output.

---

## Failure index

[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md) is the symptom→fix index: 134
entries, one `###` heading each, with `**Symptom**` (the grep string),
`**Wrong**`, `**Right**`, and `**Guard**` (the test, smoke spec or lint check
that pins it). It is the only actively maintained institutional memory in this repo and
it is grep-first by design — grep it BEFORE writing code that matches a
symptom.

Standing process rule: **when a symptom matches an existing entry, fix at
that layer first.** If the entry describes a different fix pattern than the
one the immediate code tempts you toward, the entry wins — it was written
because the tempting fix already failed. Add a new entry only after a NEW
root cause is confirmed AND a regression test exists for it.
