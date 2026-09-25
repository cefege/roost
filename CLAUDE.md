# CLAUDE.md — operating rules for Roost v3

You (an LLM agent) are the primary reader, writer, and maintainer of this
codebase. Every claim in this file was verified against code the day it was
written; if you find one that is false, fix this file in the same change.
The per-crate file map lives in [`crates/README.md`](crates/README.md), not
here — this file is operating rules only, so it cannot rot into a stale copy
of the filesystem.

**This is the `v3` branch: a complete Rust rewrite of Roost.** `main` holds
v2 (Bun + TypeScript + SolidJS) and stays in production, receiving only small
fixes. Both live in the same repository — `~/repos/roost` is `main`,
`~/repos/roost-v3` is the `v3` worktree. The v2 TypeScript tree (`apps/`,
`packages/`) is still present here while the port runs and is deleted in
Phase 7, so a mixed stack is expected and the Playwright oracle in `smoke/`
is deliberately stack-agnostic.

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
   stay byte-exact through Phase 6 so any mix of Rust and TypeScript
   components interoperates in tests.
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

## Main → v3 sync policy

v2 keeps shipping on `main` while this branch is ported, so fixes land in two
places. After each fix lands on `main`:

```sh
git -C ~/repos/roost-v3 merge main
```

A **modify/delete conflict on a TypeScript file already deleted in v3** is not
a merge to resolve by picking a side. Keep the deletion and port the behavior
to the owning Rust crate in the same merge commit, then list every ported fix
in that commit's body. A `main` fix that silently disappears with the deleted
file is a regression in production's successor, and the body line is what makes
it auditable.

A conflict in `protocol/`, `docs/`, or `smoke/` resolves normally: those trees
are shared, not forked.

---

## Repository layout and layers

```text
Cargo.toml            workspace; [workspace.dependencies] pins every crate
rust-toolchain.toml   the pinned stable toolchain
xtask/                `cargo xtask lint` — the repo gates
third_party/          vendored crates, only when a phase needs a patch
protocol/             proto/, spec/, conformance/ — the wire contract
crates/               the v3 product (see crates/README.md)
smoke/                Playwright oracle, TypeScript, run by Bun
apps/ packages/       the v2 TypeScript tree, deleted in Phase 7
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
   the baseline may never exceed 400. The v3 baseline starts empty. After a
   split lowers a count, re-snapshot with
   `cargo xtask lint --update-size-baseline`. A file that is deliberately over
   the cap records the reason in its own header and its line count in the
   baseline — the v2 case to understand is `apps/web/src/renderer/cellRenderer.ts`,
   one class whose methods share private per-frame state, where that
   encapsulation is what prevents the history-corruption class. Do not "fix"
   it by splitting; in Rust it becomes one `CellGridRenderer` struct that
   sibling modules take by `&mut`.

2. **`#![forbid(unsafe_code)]` in every crate root.** The exceptions are
   `roost-keeper`, which owns raw file descriptors and the controlling-TTY
   handshake, and anything under `third_party/`. Every `unsafe` block in
   `roost-keeper` names the invariant it protects.

3. **No panics on untrusted input.** `clippy::unwrap_used` and
   `clippy::expect_used` are denied workspace-wide
   (`[workspace.lints.clippy]` + `clippy.toml`); tests and fixtures are
   exempt via `allow-unwrap-in-tests`. A `BadEvent` from a peer is a returned
   `Err`, never a panic: a panic in the coordinator or the keeper is a
   fleet-visible outage. `todo!` and `unimplemented!` are denied too — a
   commit means the code is wired end-to-end.

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

## Porting rules

Every module ported from `apps/` or `packages/` into `crates/` follows these.

1. **Mirror the domain folder.** a coordinator module named `sync` →
   the `sync` module of the `roost-coord` crate, `apps/worker/src/keeper/` →
   `crates/roost-keeper/src/`. Read the TypeScript module, its tests, and any
   `docs/FAILURE-INDEX.md` entry naming it before writing Rust.
2. **Port behaviour, not wording.** Behaviour tests only (`#[cfg(test)]` or
   `crates/<x>/tests/<mirror>.rs`). Do not port a test that asserts on source
   text, message wording, or an incidental default — re-pin the behavior
   instead, and delete the TypeScript test when its module goes.
3. **Rewrite each Guard the moment it is ported.** For every
   `docs/FAILURE-INDEX.md` entry whose **Guard** names a TypeScript test or
   lint, add the Rust guard and rewrite that entry's **Guard** line to the
   Rust path in the same commit. An entry pointing at a test that no longer
   exists is worse than no entry.
4. **Delete the TypeScript module only when its Rust replacement passes that
   phase's gate.** Until then the mixed stack has to build, and the Playwright
   oracle has to run.
5. **Name every dropped path in the commit body.** Only paths unreachable in
   an all-v3 fleet are dropped. The list so far, all removed in their
   respective phase commits: the Windows update broker
   (`packages/host/src/windows/windows-update-broker.ts`), the legacy unimplemented `Sync`
   server-streaming RPC, the legacy `/w/:workspaceId[/t/:channelId]` routes,
   `client-seq.txt`, the keeper "Bun ABI" identity field, the Bun-specific
   zlib workaround, and capability fallbacks for peers lacking a capability
   every v3 peer advertises.

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
roost status                 # services, network, fleet: Tailscale state,
                             # listeners, cert, worker reachability
roost doctor --since 24h     # anomaly digest from local logs + audit_log
```

`roost status` is the current-state gate; `roost doctor --since <window>`
summarizes a time window and is the right tool for "what broke overnight".
Both read v3 JSON logs and the `audit_log` table, and both are documented
with example output in [`GETTING_STARTED.md`](GETTING_STARTED.md). Their
Rust implementations land in the `roost-cli` crate in Phase 6; until then the
commands run from the v2 tree at `apps/roost-cli/src/status.ts` and
`apps/roost-cli/src/doctor.ts`.

Coord down → workers redial and browsers lose state and terminal fan-out, but
keeper subprocesses preserve the PTYs until the coordinator returns. Worker
down → that machine's PTYs are unavailable; other machines keep working.

**v3 runs beside v2, not on top of it.** v3 uses the data directories
`RoostCoordinatorV3` and `RoostWorkerV3`, binds the coordinator to
`127.0.0.1:4113`, and serves the worker's local door on `127.0.0.1:4114`, so
both can be running on one machine during the port.

---

## Process

Move TypeScript files with `bun scripts/move-modules.ts --manifest …`; update
callers and path-bound docs in the same change. Rust modules move with an
ordinary `git mv` plus, if the crate's edges change, an edit to
`xtask/src/crate_dag.rs` in the same commit.

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
tier, and the one that must keep working across every mixed-stack
combination: each Playwright worker starts a real coordinator, worker, keeper
and PTYs (`smoke/terminal/stack.ts`) and drives a real browser against a built
web bundle. The stack it launches is chosen by environment, so the same
specs are the oracle for a TS stack, a Rust worker, a Rust coordinator, and
finally the all-Rust stack:

```sh
ROOST_SMOKE_COORD_EXECUTABLE=target/release/roost   # Rust coordinator
ROOST_SMOKE_WORKER_EXECUTABLE=target/release/roost  # Rust worker (`roost worker`)
ROOST_SMOKE_WEB_DIST=<dx public dir>                # Rust/Dioxus bundle
```

Unset means "use the TypeScript implementation", which is what makes the
mixed-stack runs meaningful. Coverage, by path and test name:

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
bundles. In TypeScript it is installed only when the bundle was built with
`VITE_ROOST_SMOKE=1` and `localStorage.roostSmoke === "1"`; in Rust it is
behind the `smoke` cargo feature of `roost-web`, which a release build does
not enable. The production bundle must contain no `__smoke`.

`bun run test:upgrade` is the other real-flow tier and the only gate that
proves an EXISTING install survives a new release: it stages the previous
release and the newest release tag as git worktrees, boots the working tree's
coordinator over the database that release created, opens two PTYs, deploys
through the product's own keeper-update admission
(`smoke/upgrade/release-handoff.ts`), and fails if the keeper's pid or either
channel count moved, if a marker stopped painting, or if the upgraded worker
never reported the keeper runtime the next upgrade admits on.
`test:terminal` proves a FRESH stack works and cannot see that class of defect
at all. This tier is re-created for Rust→Rust releases in Phase 7; it is off
on this branch until the first `v3.*` tag exists.

`smoke/terminal/live-stack.ts` is the hands-on escape hatch, never a gate. It
holds the same working-tree stack open and prints `READY <url> worker=<fp>`;
no tailnet. `bun run test:live-api` and a physical-phone pass watch production
only — OPTIONAL, outside the definition of done, never a merge blocker.

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

TypeScript gates, which keep running until Phase 7 deletes that tree:

```
bun run lint            # gate — scripts/lint-roost.ts, blocking in CI
bun run test:unit       # gate — hermetic unit tier; runs test:worker first
bun run test:worker     # gate — per-file isolated worker suite
bun run test:terminal   # gate — real coord + worker + keeper + PTY + browser
bun x tsgo -p tsconfig.base.json --noEmit   # gate — exactly what CI typechecks
bun run test:live-api   # optional monitor — deployed coord (ROOST_COORD_URL)
```

CI (`.github/workflows/ci.yml`) runs the Rust `workspace` job and the
TypeScript `invariants` job on ubuntu-latest AND macos-latest, then the
`terminal` job as its own matrix job on both. The `upgrade` and `wterm-wasm`
tiers and `release.yml` are removed on this branch and return in Phase 7. The
`windows-2022` tier stays behind the `ROOST_WINDOWS_GATE` repository variable
(off by default) — Windows is paused on `main` too, and v3 ships Linux and
macOS. No gate needs a deployed coordinator, a tailnet, or a human driving a
browser.

---

## Failure index

[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md) is the symptom→fix index: 99
entries, one `###` heading each, with `**Symptom**` (the grep string),
`**Wrong**`, `**Right**`, and `**Guard**` (the lint rule or test that pins
it). It is the only actively maintained institutional memory in this repo and
it is grep-first by design — grep it BEFORE writing code that matches a
symptom.

Standing process rule: **when a symptom matches an existing entry, fix at
that layer first.** If the entry describes a different fix pattern than the
one the immediate code tempts you toward, the entry wins — it was written
because the tempting fix already failed. Add a new entry only after a NEW
root cause is confirmed AND a regression test exists for it.
