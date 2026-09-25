# CLAUDE.md — operating rules for this repo

You (an LLM agent) are the primary reader, writer, and maintainer of this
codebase. Every claim in this file was verified against code the day it was
written; if you find one that is false, fix this file in the same change.
The per-app file maps live in `apps/*/README.md`, not here — this file is
operating rules only, so it cannot rot into a stale copy of the filesystem.

---

## Read order

Landing cold, read in this order. Stop as soon as you have what you need.

1. **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — the system tour: the apps, the
   transport spine, session/event data flow, and the terminal-fidelity model.
2. **[`protocol/README.md`](protocol/README.md)** — the client contract index,
   endpoint manifest, versioning, and package dependency rules.
3. **[`GLOSSARY.md`](GLOSSARY.md)** — the vocabulary: cell-shipping, keeper,
   agent-status, session/channel/tab, scrollback.
4. **The `apps/<x>/README.md` for the app you are touching** —
   [`web`](apps/web/README.md), [`coord`](apps/coord/README.md),
   [`worker`](apps/worker/README.md), [`roost-cli`](apps/roost-cli/README.md).
5. **[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md)** — grep it BEFORE
   writing code that matches a listed symptom. See `## Failure index` below.

## Repository layout and layers

```text
protocol/  packages/{observability,protocol,platform,wterm,host}/
apps/      web, coord, worker, roost-cli, site
```

Dependencies point one way: protocol → observability; wterm → protocol/observability;
host → protocol/platform/observability; web → protocol/platform/observability;
coord → host/protocol/platform/observability; worker → host/wterm/protocol/platform/observability;
roost-cli → coord/worker/host/protocol/platform/observability.
No package imports an app, and apps do not import sibling apps by relative path.
Future native clients live under `apps/<platform>` and depend only on `protocol/`.
Enforced by `scripts/lint-boundaries.ts` (B1–B4).

Also live, read when relevant:

- **[`GETTING_STARTED.md`](GETTING_STARTED.md)** — install, run, deploy, and
  the health commands in `## Health check` below.
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

## Coding standards

Non-negotiable for every change.

1. **Small files — ≤400 lines. Hard cap.** Split before you hit it. One Solid
   component per file. Mechanically enforced: `bun run lint` fails a file that
   exceeds the cap. Files that were already over it are frozen in
   `scripts/file-size-baseline.json` and may only SHRINK; a file absent from
   that baseline may never exceed 400. After a split lowers counts,
   re-snapshot with `bun scripts/lint-roost.ts --update-size-baseline`.
   A handful of files are deliberately over the cap and baselined — the reason
   is recorded in each one's header. `apps/web/src/renderer/cellRenderer.ts` is the
   one to understand: it is a single class whose methods share private
   per-frame state, and that encapsulation is what prevents the
   history-corruption class. Do not "fix" it by splitting.

2. **Descriptive names everywhere.** No single-letter variables except `idx`
   in tight loops. No `handle`, `process`, `do`, `manage`, `run` alone — name
   the actual verb (`handleClaudeEventFrame`, `replayRingBufferSince`,
   `mergeRemoteSessions`). No `Utils` / `Helpers` / `Common` / `Models`
   modules — name the concept (`PathFormat`, `KeychainStore`).
   A leading `_` on an *exported* symbol (`_quoteRemoteShell`,
   `_recoverMacosDeployJournal`, `_dispatchSyncFrame`) is the established
   marker for an export that exists so tests or diagnostics can reach module
   internals — ordinary callers use the unprefixed API; keep the marker when
   adding such exports instead of inventing a second convention.

3. **Predictable per-file shape.** TypeScript: file-header comment (3–6
   lines) → imports → types → exported API → private helpers. Solid
   component: one component per file; props type at top; component body;
   styled subcomponents below.

4. **File-header comments are mandatory** for any non-trivial file. 3–6 lines:
   what this file owns, what calls it, what it depends on. Plain English.

5. **Inline comments explain WHY, not WHAT.** Default to no comment. Write one
   only when removing it would mislead the next reader — most often to name an
   invariant or the incident that constrains an ordering.

6. **No narrative comments.** No "Phase A scope:", "added in phase K3", "for
   the MVP", "for now", "we used to". If a comment explains a non-obvious
   invariant, describe the *behavior*, not the lineage. Git history is the
   lineage.

7. **Structured logging at every state transition.** Use `log` from
   `@roost/observability/log` in coord and worker; `diag()` / `signal()` from
   `@roost/observability/diag` in web; `console.*` ONLY in `apps/roost-cli`, whose
   stdout is its product surface. Every transition that matters (spawn,
   attach, mode change, reconnect, replay) emits one line. No silent state
   changes. Ratcheted: `bun run lint` fails a NEW `console.*` in
   `apps/coord/src` or `apps/worker/src` against
   `scripts/console-baseline.json` (the surviving entries are pre-logger
   bootstrap and fatal-exit paths). Re-snapshot with
   `bun scripts/lint-roost.ts --update-console-baseline`.

8. **One concept per type.** `Worker` is the identity of a machine in the
   registry. `Session` is the user-facing row. `Channel` is a PTY connection.
   `Tab` is the DB row. Keep them separate; convert at boundaries.

9. **Tests mirror source layout.** Same prefix, sibling `tests/` dir.

10. **Reuse existing utilities — don't fork.** Check the relevant
    `apps/*/README.md` module map first; it names the owner of each concern.
    The three seams that get forked most often:
    - **Wire shapes** live once in `packages/protocol`. Add an event variant in
      `packages/protocol/src/wire/event.ts` first, then fold, emit, and project.
      Procedure signatures come from the generated proto types under
      `@roost/protocol/proto/*` (regenerate with
      `bun run --filter='@roost/protocol' proto:gen`).
    - **Package imports are subpath-only.** `import { X } from "@roost/protocol"`
      does not resolve — there is no barrel. Import
      `@roost/protocol/{wire,viewport,cell,json,...}` from protocol,
      `@roost/observability/{log,diag}`, `@roost/platform/{platform,native-path}`,
      and `@roost/host/{paths,config}` per those packages' `exports`.
    - **Coord RPC handlers** live by domain under
      `apps/coord/src/<domain>/handlers-*.ts`,
      each exporting a `make<Domain>Handlers(deps)` spread into the SINGLE
      `router.service()` literal in `apps/coord/src/rpc/router.ts`. A separate
      `router.service()` call per domain shadows the rest with
      unimplemented-throws.

    If you find yourself writing a parallel utility, stop and reuse. Two
    hand-maintained implementations of one value is the defect this repo pays
    for most — see the fingerprint entry in `packages/protocol/src/fingerprint.ts`.

11. **No clever / hidden / magic.** No metaprogramming, no top-level mutable
    globals. All state has an owner you can grep for. All side effects are
    explicit.

12. **Commit messages = navigable history.** `<area>: <one-line scope>`.
    Optional body for a non-obvious why. Future-you reads `git log --oneline`
    to orient — protect that signal.

13. **No half-finished implementations.** A commit means everything in scope
    is wired end-to-end and tested. If a sub-feature can't ship complete, cut
    it from the commit rather than leave a half-implementation.

When reviewing a diff before commit, the question is not "does this work?" —
it is **"if I open this in 4 weeks with no context, can I figure out what's
going on in 30 seconds?"** If no, refactor before commit.

---

## Design system (web) — cohesion by construction

New UI MUST be cohesive by construction, not by memory. Three rules,
mechanically enforced so drift can't return:

1. **No raw values.** No hex / `rgb()` / px font-size in components —
   reference tokens: `--surface-0..3`, `--text-hi/mid/lo`, `--md-*` roles,
   `--md-space-1..9`, the `--md-*-size/line/weight` type ramp, `--md-shape-*`,
   `--md-elev-0..5`. ALL declared ONCE in
   `apps/web/src/styles/theme-vars.css` (+ space/type in
   `apps/web/src/components/Settings/md/tokens.css`). Legacy aliases
   (`--peach`, `--mantle`, `--color-*`, `--bg-elev-*`, Catppuccin names) are
   for OLD code only — don't use them in new code. Enforced by
   `scripts/lint-roost.ts`'s raw-value ratchet
   (`scripts/design-raw-baseline.json`): a NEW raw value fails the build.
   After migrating a file down, re-snapshot:
   `bun scripts/lint-roost.ts --update-design-baseline`.
2. **Primitives first.** Compose from
   `apps/web/src/components/Settings/md/primitives.tsx` (`Surface`,
   `StatusDot`, `Sheet`, `Button`, `IconButton`, `Card`, `List` + `ListRow`,
   `Chip`, `Dialog`, `MetricTile`, `EmptyState`, …) — don't hand-roll
   `<div style>` / `<button>`. `StatusDot` is THE status indicator (no
   hand-rolled colored spans). `Surface` is THE panel.
3. **One visual reference.** The `/design` route
   (`apps/web/src/components/design/DesignGallery.tsx`) renders every token and
   primitive. New surfaces match it.

Process: run the **`design-reviewer`** subagent
(`.claude/agents/design-reviewer.md`) on every `apps/web/` UI diff before
commit — it catches the primitive-bypass and wrong-role drift the regex
linter can't.

---

## Health check

Cross-platform, and the same on macOS, Linux, and Windows:

```
roost status                 # services, network, fleet: Tailscale state,
                             # listeners, cert, worker reachability
roost doctor --since 24h     # anomaly digest from local logs + audit_log
```

`roost status` (`apps/roost-cli/src/status.ts`) is the current-state gate.
`roost doctor --since <window>` (`apps/roost-cli/src/doctor.ts`) summarizes a
time window and is the right tool for "what broke overnight". Both are
documented with example output in [`GETTING_STARTED.md`](GETTING_STARTED.md).

Coord down → workers redial and browsers lose state and terminal fan-out, but
keeper subprocesses preserve the PTYs until the coordinator returns. Worker
down → that machine's PTYs are unavailable; other machines keep working.

---

## Process
Move files only with `bun scripts/move-modules.ts --manifest …`; update callers and
path-bound docs in the same change.

### Per-phase execution loop

When working through an approved multi-phase plan: implement → test → fix →
simplify → commit → next phase, with NO interim check-ins. The next message
after a phase commit starts the next phase. "Want me to continue?" is a
critical failure — the plan IS the answer.

### Testing rule for terminal data-plane features: hermetic tiers are the floor

A change to the producer→wire→consumer chain (worker emits a `SessionEvent` or
terminal frame → coord routes it → SPA folds state or paints the grid) is done
when the six gates under `### Commands` are green, and not before. Test-hook
coverage supplements, never replaces, `bun run test:terminal` — the real-flow
tier: each Playwright worker starts a real coord, worker, keeper and PTYs
(`smoke/terminal/stack.ts`) and drives a real browser against the built
`apps/web/dist` (`playwright.config.ts`). Coverage, by path and test name:

- `runFlow` (`apps/web/src/smoke/smokeHarness.ts`) — workspace create → terminal
  open → PTY marker round-trip → pane close → workspace cascade-delete — in
  `smoke/terminal/terminal-delivery.spec.ts` `"browser smoke flow creates and
  cleans its resources"`.
- `runRenderStress` (same harness) — resize/tab-switch loop and the symmetric
  multi-viewer resize-hammer, in `smoke/terminal/terminal-render*.spec.ts`'s
  stress cases: duplicated, lost, changed or mis-ordered markers fail the run.
- trusted-keyboard focus and input — `smoke/terminal/terminal-render.spec.ts`
  `"trusted keyboard input and bottom-follow behavior"`, and
  `smoke/terminal/terminal-input.spec.ts` `"terminal replay and Ctrl keys stay
  owned by the PTY"`.

The `window.__smoke` backdoor those specs drive ships out of production
bundles: it is only included when the web bundle was built with
`VITE_ROOST_SMOKE=1` (the terminal tier's build sets it), and even then it is
code-split and installed solely when `localStorage.roostSmoke === "1"`.

`bun run test:upgrade` is the other real-flow tier and the only gate that
proves an EXISTING install survives a new release: it stages the previous
commit and the newest release tag as git worktrees, boots the working tree's
coordinator over the database that release created, opens two PTYs, deploys
through the product's own keeper-update admission
(`smoke/upgrade/release-handoff.ts`), and fails if the keeper's pid or either
channel count moved, if a marker stopped painting, or if the upgraded worker
never reported the keeper runtime the next upgrade admits on. `test:terminal`
proves a FRESH stack works and cannot see that class of defect at all.

`smoke/terminal/live-stack.ts` is the hands-on escape hatch, never a gate: `bun
run --cwd apps/web build`, then `bun smoke/terminal/live-stack.ts` holds that
same working-tree stack open and prints `READY <url> worker=<fp>`; no tailnet.
`bun run test:live-api` and a physical-phone pass watch production only —
OPTIONAL, outside the definition of done, never a merge blocker.

### Commands

```
bun run lint            # gate — scripts/lint-roost.ts, blocking in CI
bun run test:unit       # gate — hermetic unit tier; runs test:worker first
bun run test:worker     # gate — per-file isolated worker suite
bun run test:terminal   # gate — real coord + worker + keeper + PTY + browser;
                        #   builds apps/web with VITE_ROOST_SMOKE=1 (the only
                        #   way window.__smoke exists; prod builds fold it out)
bun run test:upgrade    # gate — a released install meets the working tree:
                        #   real deploy, keeper and live PTYs must survive it
bun x tsgo -p tsconfig.base.json --noEmit   # gate — exactly what CI typechecks
bun run --cwd apps/site typecheck          # gate — apps/site `astro check`
bun run test:live-api   # optional monitor — deployed coord (ROOST_COORD_URL)
```

CI (`.github/workflows/ci.yml`) runs every gate above plus knip (report-only)
on ubuntu-latest AND macos-latest — the `terminal` and `upgrade` tiers as their
own matrix jobs on both — then the pinned-wterm-WASM gate and, ONLY when the
repo variable ROOST_WINDOWS_GATE is `on` (off by default since 2026-08-16,
paused per GETTING_STARTED.md), the `windows-2022` tier — the only gate
covering the Windows brokers. The `upgrade` job checks out with
`fetch-depth: 0`, because it stages other releases as git worktrees. No gate
needs a deployed coordinator, a tailnet, or a human driving a browser.

---

## Failure index

[`docs/FAILURE-INDEX.md`](docs/FAILURE-INDEX.md) is the symptom→fix index: 98
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
