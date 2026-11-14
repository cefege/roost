# OMP terminal / HTML parity contract

OMP is Roost's only agent. The reference is the fork pinned in `README.md`,
including the ordered patches in `source.json`. Parity covers its whole
interface, not only chat, model selection, or a list of smoke checks.

## One implementation of behavior

Both renderers use OMP's actual session, `InteractiveMode`, controllers,
command registry, components, settings store and extension API. The browser
renders OMP's native component descriptions and returns native events. It
does not interpret commands, invent settings choices, settle dialogs, or
implement another agent harness. Roost's navigation and machine/session
management remain Roost UI, not a second implementation of OMP preferences.

A component's terminal renderer and native description are two adapters of
the same state. Neither adapter's output is an oracle for the other. A shared
bug in their descriptions must not make a differential check pass.

## Reference inventory

Coverage is derived from the pinned fork's source and runtime registries:

- Every TSP kind, declared property and native event family.
- Every built-in command, alias and registered extension command.
- Every component, dialog, picker, editor, settings scene and host service
  reachable from the OMP interface.
- Chat, streaming, thinking, tool lifecycle/output, media, notifications,
  errors, cancellation, keyboard/focus behavior and persisted state.

Inventory entries identify behavioral scenarios, not merely a renderer arm
or a matching name in a test file. New kinds, fields, commands or interface
components without exercised scenarios fail the gate. Runtime extensions
must supply the native interface for their custom interactive surfaces;
terminal-only or unknown components are explicit failures, not successful
blank output or a plain-chat fallback.

## Differential evidence

Each scenario starts two isolated copies of the same seeded OMP state. One
runs the real TUI through a PTY; the other runs the real HTML application.
Neither reads the operator's home, credentials, conversations or services.
A local fixture supplies deterministic provider responses, tool results and
host-service outcomes. No paid inference or external login is required.

The runner performs equivalent terminal and browser actions and records:

1. Actual terminal cells and actual visible DOM content, accessible names,
   control values, available actions and disabled/error/loading states.
2. The resulting OMP state, native events and controller transitions.
3. Settings and session persistence, including reopening after mutation.

Assertions compare those independent observations at each meaningful
transition. They must detect missing or repeated information, missing
controls, wrong choices, wrong selection, swallowed keys, canceled actions
that still execute, and mutations that disappear after reopening. A protocol
snapshot, screenshot, test count, or two reads of the same semantic tree is
not sufficient evidence.

## Browser differences

| Area | Required HTML behavior |
| --- | --- |
| Layout | Responsive flow, wrapping and native scrolling; no terminal-cell geometry requirement. |
| Text | Selectable content and native selection/copy without losing OMP information. |
| Controls | Accessible names, focus, keyboard operation and touch targets. |
| Links and files | Native browser/host actions with visible success, cancellation or failure. |
| Clipboard | Browser permission handling without silently changing the OMP action. |
| Viewport | Desktop and compact/mobile preserve content, actions and state. |

Comparison normalizations are explicit and narrow: terminal box drawing,
layout padding/wrapping and fixture-owned ephemeral identities. They cannot
remove arbitrary text, errors, controls or state differences. Time-dependent
values require a controlled clock or a stated numerical tolerance.

## Completion gate

The gate fails on uncovered inventory, unsupported components, transport or
browser errors, differential mismatches, missing evidence, and incomplete
scenario execution. Skips and expected failures are not passing coverage.
Every browser fallback that cannot render an OMP node (an unknown kind,
settings control or choice style, hover-card component, or media type)
carries `data-omp-render-error`; one such element at any checkpoint fails the
case.
Both CI and release verification run it against the same pinned fork.

Completion requires the entire inventory to pass, real desktop and mobile
interaction proof, updated living contracts and an owned commit. Any claim
of parity names the fork revision and scenario inventory it actually ran;
it is not a claim to prove every possible future extension or input.

## Running the gate

`cargo xtask omp-parity -- <flags>` (or `bun run run.ts <flags>` in
`deploy/omp/parity` after a product build) runs the scenarios. Each case gets a
fresh stack, an isolated HOME and deny-all networking.

| Flag | Effect |
| --- | --- |
| `--case ID` / `--match RE` | Focused run; keeps raw probe traces for debugging and makes no coverage claim. |
| `--shard K/N` | Runs the K-th of N deterministic slices of the full inventory. |
| `--jobs N` | Runs up to N cases concurrently (1–16); cases that bind the same fixed port never overlap. |
| `--merge DIR` | Repeatable; merges shard results and evaluates coverage over the whole inventory. |
| `--list` | Prints the case ids. |
| `--output DIR` | Keep the directory under this checkout's `target/omp-parity/`; instrumentation rejects traces outside that owned root. |

Only a full run or a merge of every shard of one inventory decides coverage.
Shard and full runs keep compacted provenance in each case's `result.json`
(identical probe visits collapse within one process window bounded by
checkpoint cursors) and delete a passing case's raw `probe.jsonl`.

CI (`.github/workflows/omp-parity.yml`) builds the product once, runs 32
shards with `--jobs 2` against that build, then merges every shard's results
into the coverage verdict.

## Frozen-conversation picture passes

Use separate isolated HOME copies for TUI and HTML; never resume the operator's
original conversation. Match source elements and disclosure states, not scroll
percentages. Include checkpoints before compaction: a short current window does
not exercise older, larger transcript costs.

Record typing, scrolling, navigation and native-update frame times, long tasks
and rendered-node counts. Unchanged history must not be reconstructed for composer
or unrelated store updates. Reuse must preserve changed text, images, focus and
controller events; exercise session switches and disclosure controls as well.
A conversation without images or ranked Find results needs the corresponding
native-component scenarios too.
One frozen conversation is evidence for its elements, not a full parity verdict.

## Transcript presentation

The HTML transcript follows the compact activity-row and reading-column patterns
in [pi-pocket](https://github.com/TannerMidd/pi-pocket/tree/5d009fa4d1e2b69db54f05e04f750a6f2e591bb1),
adapted to Roost's stock Rust/UI components:

- History and composer share the responsive `max-w-reading` column.
- Tool calls use borderless rows, with verb, status, target and quiet actions in
  one heading; details sit underneath. They do not add a padded Card around
  code, output or diffs.
- Disclosure chevrons reflect the native controller's state. Long target labels
  truncate visually, retaining their full title, source and link action.
- Prose uses the default Markdown body size. Code/output/diff copy controls use
  the stock compact icon button and retain the clipboard hook's success state.

For presentation acceptance, inspect real frozen-history views at desktop and
phone widths. Exercise disclosure and exact-source copying, including commands
that wrap on phones; check page overflow and render errors. A read-only picture
pass requires no inference or operator credentials. Isolated model metadata can
point at an owned loopback deny endpoint; unexpected inference requests fail
acceptance.
