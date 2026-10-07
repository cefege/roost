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
`~/repos/roost-v3` on `v3`. Only Rust lives in this repository.

---

## Read order

Landing cold, read in this order. Stop as soon as you have what you need.

1. **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — the v3 system tour: the
   components, the transport spine, session/event data flow, and the
   terminal-fidelity model.
2. **[`crates/README.md`](crates/README.md)** — the crate map and the
   dependency DAG that `cargo xtask lint` enforces.
3. **[`protocol/README.md`](protocol/README.md)** — the client contract index,
   endpoint manifest, and versioning. The `.proto` files and `protocol/spec/`
   are the contract the Rust crates build against.
4. **[`GLOSSARY.md`](GLOSSARY.md)** — the vocabulary: cell-shipping, keeper,
   agent-status, session/channel/tab, scrollback.
5. **[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md)** — grep it BEFORE
   writing code that matches a listed symptom. See `## Failure index` below.

Also live, read when relevant:

- **[`GETTING_STARTED.md`](GETTING_STARTED.md)** — install, run, deploy, and
  the health commands in `## Health check` below, for v3.
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
   not be `macro_rules!`-generated, and `crates/roost-coord/tests/suite.rs`,
   the generated one-binary test root, which outgrew the cap at two lines per
   test file. An exempt file is neither counted nor
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

   Each crate's integration tests compile as ONE binary, `tests/suite.rs`,
   whose crate-level allow covers every test module and fixture.
   `cargo xtask lint --update-test-suites` regenerates it after adding or
   removing a `tests/*.rs` file.

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
for peers lacking a capability every v3 peer advertises, the whole v2
TypeScript product tree (`apps/`, `packages/`, the product `scripts/`), and
the v2 Homebrew formula (`Formula/`), which a tap reads from the default
branch, not from `v3`; v3 installs through `install.sh`.

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

**Our fleet's coordinator is not a host service.** It runs on a single-node
k3s on ovh1, the same host as the public edge (namespace `roost`, Deployment
`roost-coordinator`, StatefulSet `roost-coordinator-postgres`, a daily
`pg_dump` CronJob keeping 14 archives), so on every fleet host `roost status`
reports the coordinator service and listener as absent; that is expected. The
k3s API answers on ovh1's tailnet address to desktop-pc only; desktop-pc's
kubeconfig for it is `~/.kube/ovh1.yaml`. Check it with:

```
export KUBECONFIG=~/.kube/ovh1.yaml
kubectl -n roost get pods                         # coordinator, postgres, backups
curl -s https://mike.roosttt.com/readyz           # 200 = database answers
kubectl -n roost logs deploy/roost-coordinator --since=1h   # the JSON log
kubectl -n roost exec deploy/roost-coordinator -- roost doctor --since 24h
```

ovh1's `roost-saas-legacy-bridge` reaches it through the drop-in
`/etc/systemd/system/roost-saas-legacy-bridge.service.d/ovh1-k3s.conf`
(`100.103.95.19:30413`). desktop-pc's k3s still holds the previous release,
scaled to 0, with its Postgres as of the move. Roll back to it: on desktop-pc
`kubectl -n roost scale deploy/roost-coordinator --replicas=1`
(`KUBECONFIG=/etc/rancher/k3s/k3s.yaml`), and on ovh1 point the drop-in at
`100.66.192.24:30413` and restart the bridge. Rows written on ovh1 since do not
carry back unless dumped and restored (`pg_dump -Fc` / `pg_restore --no-owner`
between the two `roost-coordinator-postgres-0` pods).

**v3 runs beside v2, not on top of it.** v3 uses the data directories
`RoostCoordinatorV3` and `RoostWorkerV3`, binds the coordinator to
`127.0.0.1:4113`, and serves the worker's local door on `127.0.0.1:4114`, so
both can be running on one machine during the port.

---

## Process

Rust modules move with an ordinary `git mv` plus, if the crate's edges change,
an edit to `xtask/src/crate_dag.rs` in the same commit; update callers and
path-bound docs in the same change. Never run a gate in a worktree someone
else is editing.

### Per-phase execution loop

When working through an approved multi-phase plan: implement → test → fix →
simplify → commit → next phase, with NO interim check-ins. The next message
after a phase commit starts the next phase. "Want me to continue?" is a
critical failure — the plan IS the answer.

### Commands

The per-change gates. They run on GitHub (`ci.yml`, on every push and pull
request to `v3`); push a branch and open a pull request to run them. On
desktop-pc, which runs the production k3s workloads, run only scoped
commands (below), never the workspace-wide ones:

```
cargo xtask fmt             # cargo fmt --check, over the crates we author
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace   # unit + integration suites, one process per test
cargo test --workspace --doc    # doctests; nextest does not run them
cargo xtask lint            # 400-line cap, crate DAG, stdout rule, one test
                            #   binary per crate, design raw-value ratchet
```

Plain `cargo test` on a crate's integration suite is unsupported: its tests
share one process there, and several install process-global state. Use
nextest. One test: `cargo nextest run -p <crate> <module>::<test>`.

Before a release tag, `release.yml`'s `verify` job runs the same gates plus
the vendored terminal core's suite
(`cargo test --manifest-path third_party/alacritty_terminal/Cargo.toml`; it is
not a workspace member, because `cargo fmt` walks local path dependencies
regardless of `exclude`) and the wasm32 build of the browser crates.

CI (`.github/workflows/ci.yml`) runs the `rust` job on ubuntu-latest AND
macos-latest. `.github/workflows/release.yml` publishes a `v3.*` tag: it
re-runs the `rust` job's commands and builds four triples and the web bundle,
whose assets `install.sh` and `cargo xtask fleet install` fetch. No gate
needs a deployed coordinator, a tailnet, or a human driving a browser. The
repository is public, so GitHub-hosted runners cost nothing.

### Build discipline

**desktop-pc is a production host.** Its k3s runs Immich, Nextcloud, Forgejo,
Home Assistant and monitoring. Release builds,
workspace-wide gates, stress runs and load generators do not run there: they
run on GitHub. An agent on desktop-pc runs only scoped commands
(`cargo check -p <crate>`, `cargo clippy -p <crate>`,
`cargo nextest run -p <crate> <module>`), never starts a process that exists
to burn CPU (`yes`, `stress-ng`, `--stress-count`), and reproduces a flaky
test on CI rather than locally. CPU-burning processes an agent started there,
outside the cargo cap, once held the CPUs at 75 % pressure for half an hour and
slowed every production service.

**One cargo command at a time per machine.** On desktop-pc,
`~/.local/bin/cargo` enforces this itself — a machine-wide lock, CPUs 0–3,
inside `rust-build.slice` — so plain `cargo …` is correct there; it caps cargo
and its children only, not anything else an agent starts. On every other
machine an agent wraps every `cargo`/`dx` invocation as
`flock /tmp/roost-cargo.lock cargo …`. **Never `cargo clean` a workspace on
desktop-pc**: sccache plus the warm `target/` make it unnecessary.

**Each worktree builds into its own `target/`.** Never point two worktrees at
one `CARGO_TARGET_DIR`. Cargo identifies a path crate by its
workspace-relative path and decides freshness by mtime, so a shared directory
reuses artifacts built from another tree's sources, and gates then pass or
fail on the wrong code. `cargo xtask` run from a worktree needs
`ROOST_REPO_ROOT=<worktree>`.

**Dependencies come from sccache.** Each fleet machine with a toolchain sets
`[build] rustc-wrapper` in `~/.cargo/config.toml` to sccache, which keeps a
local disk cache shared by every checkout. Third-party libraries and C objects
hit across worktrees. Proc-macros, build scripts, crates that read `OUT_DIR`,
and the incremental workspace crates recompile per worktree.
`sccache --show-stats` shows the hit rate.

### Release to the fleet

Our own machines take a release from GitHub: `release.yml` builds it, this
checkout installs it. The host list, install order and service names are
`xtask/fleet.json`.

```
git tag <tag> && git push origin <tag>      # release.yml + container.yml build everything
cargo xtask fleet install --version <tag> [--host <name>]…
```

`fleet install` downloads the tag's release assets with `gh` into
`target/fleet/<tag>/` (the Linux x64 and macOS arm64 pairs and the web
bundle), checks each against its `.sha256`, and records the commit the tag
names; it refuses until `release.yml` has published the release. It then
upgrades the coordinator, which runs on ovh1's k3s against an in-chart
Postgres (`fleet.json` `coordinator`, values in
`deploy/helm/fleet-ovh1.values.yaml`): it refuses until
`.github/workflows/container.yml` has published the tag's image to ghcr, then
`helm upgrade`s and checks the pod reports the tag and sha. It then copies the
tag into each host's `versions/<tag>/`, repoints the systemd units or the
LaunchAgent at it, restarts them, and fails a host whose binary reports
another commit or whose keeper pid changed. The public door is unchanged:
`mike.roosttt.com` reaches ovh1's edge Caddy, whose `roost-saas-legacy-bridge`
forwards to the coordinator's NodePort (30413) on ovh1's tailnet address.
Needs `gh`, `helm` and `kubectl` on this machine.

---

## Failure index

[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md) is the symptom→fix index: 143
entries, one `###` heading each, with `**Symptom**` (the grep string),
`**Wrong**`, `**Right**`, and `**Guard**` (the test or lint check
that pins it). It is the only actively maintained institutional memory in this repo and
it is grep-first by design — grep it BEFORE writing code that matches a
symptom.

Standing process rule: **when a symptom matches an existing entry, fix at
that layer first.** If the entry describes a different fix pattern than the
one the immediate code tempts you toward, the entry wins — it was written
because the tempting fix already failed. Add a new entry only after a NEW
root cause is confirmed AND a regression test exists for it.
