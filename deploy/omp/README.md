# OMP command-UI parity build

Workers that host the semantic command UI need an `omp` built from the pinned
fork, not a stock release:

- base commit `650f3a108229ee11bf68ba3377f4e8c3391cd424` of our
  `omp-command-ui` fork (github.com/badlogic/pi-mono lineage, private mirror)
- `command-ui-parity.patch` in this directory, applied on top
  (`git apply command-ui-parity.patch`), then `bun install && bun run build`
  in `packages/coding-agent`
- install the resulting `pi`/`omp` binary ahead of any release `omp` on the
  worker `PATH` (the worker resolves the binary through
  `roost-agent-tools::search_path`, so `~/.local/bin` or the session cwd's
  `node_modules/.bin` both win over a package-manager install)

The patch adds the hosted TSP command-UI extension (`_omp/ui/*`), the browser
host-service bridge, and the native picker/settings/session descriptors the
web client projects. Without it the worker falls back to stock OMP, which
answers `_omp/ui/open` with an unsupported capability and the browser keeps
the plain chat surface.
