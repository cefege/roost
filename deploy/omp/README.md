# OMP runtime and command-UI parity

Releases from `v3.0.5` ship the pinned standalone OMP fork for every supported
worker platform. Installers, fleet installation and self-update verify its
digest and stage `omp` (`omp.exe` on Windows) beside `roost` and `roost-keeper`
under `versions/<tag>/bin/`. The worker selects that release-owned sibling
before project or user executables. Global OMP installations and credentials
under `~/.omp` are not replaced.

The source identity is [`source.json`](source.json):

- `https://github.com/cefege/oh-my-pi.git`
- base commit `650f3a108229ee11bf68ba3377f4e8c3391cd424`
- [`command-ui-parity.patch`](command-ui-parity.patch), applied on top

`bun deploy/omp/runtime.ts` prepares that exact source, provisions its matching
published native binding, compiles a standalone executable and verifies its ACP
initialization in an isolated home. Outputs and digests are under
`target/omp-runtime/`; `.github/workflows/omp-runtime.yml` builds all five native
assets before publication. `bun deploy/omp/runtime-smoke.ts <binary>` verifies
an already-built executable without rebuilding.

Source builds without a sibling retain project/PATH lookup. They still require
the patched fork; stock OMP and alternative runtimes are unsupported.
Initialization requires
`agentCapabilities._meta.commandUi = {"version":1,"protocol":"tsp"}` before a
session can open. Startup failures are displayed in chat, not hidden behind
“Connecting to OMP…”. There is no plain-chat fallback.

The fork retains the real TUI as the reference. TUI/HTML A/B checks cover the
same command, state, actions and results; HTML keeps native browser controls,
selection, clipboard and responsive layout. A partial flow is not full parity.
