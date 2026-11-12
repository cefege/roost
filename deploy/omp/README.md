# OMP command-UI parity build

Every worker that hosts OMP conversations requires the pinned OMP fork. Stock
OMP and other agent runtimes are not supported:

- base commit `650f3a108229ee11bf68ba3377f4e8c3391cd424` of our
  `omp-command-ui` checkout (`https://github.com/cefege/oh-my-pi.git`)
- `command-ui-parity.patch` in this directory, applied on top
  (`git apply command-ui-parity.patch`), then `bun install && bun run build`
  in `packages/coding-agent`
- install the resulting `pi`/`omp` binary ahead of any release `omp` on the
  worker `PATH` (the worker resolves the binary through
  `roost-host::search_path`, so `~/.local/bin` or the session cwd's
  `node_modules/.bin` both win over a package-manager install)

The patch adds the hosted TSP command-UI extension (`_omp/ui/*`), the browser
host-service bridge, and the native picker/settings/session descriptors the
web client projects. Initialization requires
`agentCapabilities._meta.commandUi = {"version":1,"protocol":"tsp"}` before a
session can open. Missing or incompatible support produces an explicit failure;
there is no plain-chat fallback.

The fork retains the real TUI as the reference. TUI/HTML A/B checks must cover
the same command, state, actions and results; HTML keeps native browser controls,
selection, clipboard and responsive layout. A partial flow is not full parity.
