# OMP runtime and command-UI parity

Release packages ship the pinned standalone OMP fork for every supported
worker platform. Installers, fleet installation and self-update verify its
digest and stage `omp` (`omp.exe` on Windows) beside `roost` and `roost-keeper`
under `versions/<tag>/bin/`. The worker selects that release-owned sibling
before project or user executables. Global OMP installations and credentials
under `~/.omp` are not replaced.

Each conversation's dedicated process receives `--cwd` before ACP starts.
`session/new.cwd` alone does not bind the native UI's process-wide project state.

The source identity is [`source.json`](source.json):

- `https://github.com/cefege/oh-my-pi.git`
- base commit `650f3a108229ee11bf68ba3377f4e8c3391cd424`
- [`command-ui-parity.patch`](command-ui-parity.patch), then
  [`transcript-fidelity.patch`](transcript-fidelity.patch), in `source.json` order

The transcript patch preserves compact thinking headings, rich Read previews,
ordered file lists, bounded agent-message previews and keyed Todo phase toggles.

HTML keeps a view-local rendered history snapshot. Composer and unrelated store
updates reuse it; history, focus, blob or sender changes invalidate it. History
is neither virtualized nor discarded.

`bun deploy/omp/runtime.ts` prepares that exact source, provisions its matching
published native binding and compiles a standalone executable. Its isolated
smoke verifies ACP capabilities, session creation, semantic UI opening and
the rendered workspace while starting outside that directory. Outputs and
digests are under `target/omp-runtime/`; `.github/workflows/omp-runtime.yml`
builds all five native assets before publication.
`bun deploy/omp/runtime-smoke.ts <binary>` verifies an already-built executable.

Source builds without a sibling retain project/PATH lookup. They still require
the patched fork; stock OMP and alternative runtimes are unsupported.
Initialization requires
`agentCapabilities._meta.commandUi = {"version":1,"protocol":"tsp"}` before a
session can open. Startup failures are displayed in chat, not hidden behind
“Connecting to OMP…”. There is no plain-chat fallback.

The fork retains the real TUI as the reference. TUI/HTML A/B checks cover the
same command, state, actions and results; HTML keeps native browser controls,
selection, clipboard and responsive layout. A partial flow is not full parity.
