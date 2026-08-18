# @roost/cli — the one CLI

`roost` is the single operator surface: install, deploy, update, inspect, test.
It replaced 7+ scattered shell scripts. Run it as `bun run roost <sub> [args]`
(root script), `bun apps/roost-cli/src/main.ts <sub>`, or as the compiled
`roost` binary — the compiled binary also uses this CLI for its *server* modes
(`roost coord`, `roost worker`, `roost keeper`).

## Entry point

`src/main.ts` holds the `SUBCOMMANDS` dispatch object. It is the source of truth
for the command surface: `main.ts` looks the argv up in that object, so a command
exists exactly when it has a key there. `--version` / `-v` alias to `version`; an
unknown key prints `usage()` and exits 1.

`SUBCOMMANDS` has **22 keys** — 21 user-facing plus one internal broker. `usage()`
prints 20 of them; `keeper` and `__windows-updater-broker` are intentionally
omitted because neither is user-invoked.

| Command | Purpose |
| --- | --- |
| `quickstart` | One-shot local install: tailscale gate → build SPA → mint TLS cert → install coord → deploy local worker → health → open the browser already authorized via a self-minted `#pair` token |
| `coord` | Run the coordinator in this process (compiled-binary server mode). Lazily `import()`ed so the generated SPA embed never loads into `roost test` |
| `worker` | Run the worker in this process (compiled-binary worker mode); same entry the LaunchAgent/unit uses |
| `keeper <sock>` | Run the multiplexed keeper in this process. Internal self-exec target: the worker spawns `roost keeper <sock>` when it is not running under bun |
| `update` | Self-update the binary from the latest GitHub release |
| `__windows-updater-broker` | **Internal, win32-only.** See below — its name and argv are a contract |
| `version` | Print the version / build identity (`--version`, `-v`) |
| `expose <hostname>` | Put the coordinator behind Cloudflare Access (`--team`, `--aud`, `--config`) |
| `dev` | Boot coord (:4102) + outbound-only worker + web dev server (:5174) in parallel |
| `test [profile]` | Canonical test entry point; profiles `unit`, `worker`, `terminal`, `live-api`, `all` |
| `deploy <host>` | Refresh the worker on a tailnet host (macOS rsync + LaunchAgent, Linux in-place checkout) |
| `push` | Publish one clean commit, deploy every registered worker, update the coordinator's own checkout, and prove every process reports that commit before returning success |
| `keeper-refresh <host> --yes` | Re-spawn a host's keeper on current code. Destructive, explicitly confirmed, and the only workflow authorized to stop a keeper |
| `logs <coord\|worker> [--tail N]` | Tail an app's log files; warns past 100 MB |
| `reset` | Stop both services, wipe the coord DB + pinned keys + lock, re-run `bun install` |
| `state` | Print a `STATE.md` snapshot to stdout |
| `cutover` | Migrate `coordinator.db` → `coordinator_v2.db` |
| `status` | ✓/✗ health readout (tailscale gate, both services, coord liveness, workers), each line carrying its own remedy |
| `doctor [--since 24h]` | Anomaly digest from the low-volume Tier-1 channel (`main.err.log` + rotated `.N.gz`) |
| `api <verb>` | Headless introspect/drive over the coord Connect RPCs: `sessions`, `cells`, `input`, `rename`, `assign`, `attach`, `spawn`, `kill`, `workers`, `workspaces`, `ws-*`, `tasks`, `task-*`, `ui`, `ui-state`, `events` |
| `join` | Install + register this machine's worker from a one-shot bootstrap token (driven by `join.sh`; needs `ROOST_COORDINATOR_URL` + `ROOST_BOOTSTRAP_TOKEN`) |
| `add-machine --platform <macos\|linux\|windows>` | Mint one worker token and print the platform-specific enrollment command. Coordinator-only |

### `__windows-updater-broker` is a contract, not an implementation detail

Its argv is pinned in `assets/windows/service-templates.json` and
identity-checked in `native/windows/roost-win-helper.cpp`, so renaming the key or
changing its argv shape breaks the native helper's admission check. The handler
refuses to run unless `process.platform === "win32"` and `args.length === 0`. It
loads every Windows module by dynamic `import()` so the native-helper-dependent
code never enters a POSIX command path. It drains pending relocation requests
(bounded at 16), runs the relocation broker for `worker-endpoint` and
`coordinator-promotion`, and only then admits and runs the update broker.

## Module map

- **Dispatch** — `src/main.ts`.
- **Deploy / push (POSIX)** — `src/deploy.ts` (macOS orchestrator + journalled
  recovery), `src/deploy-linux.ts` (in-place git checkout, not an rsynced slim
  tree), `src/deploy-local.ts`, `src/deploy-exec.ts` (the ssh/spawn layer and the
  shared ssh option set), `src/deploy-plist-env.ts`, `src/deploy-self-host.ts`,
  `src/push.ts`, `src/keeper-refresh.ts`.
- **Windows-only** — `src/windows/`: `windows-update-broker.ts` (the elevated
  update state machine), `windows-update-journal.ts` (durable `update-v2.json`
  journal + progress ring), `windows-update-control.ts` (request admission +
  progress read), `windows-update-runtime.ts` (native + health-prover bindings),
  and the relocation trio `windows-relocation-broker.ts`,
  `windows-relocation-control.ts`, `windows-relocation-journal.ts`.
- **Install + service control** — `src/service-ctl.ts`,
  `src/install-binary-agents.ts`, `src/machine-transaction.ts`, `src/join.ts`,
  `src/add-machine.ts`, `src/quickstart.ts`, `src/expose.ts`.
- **Release** — `src/update.ts`, `src/version.ts`.
- **Diagnostics** — `src/status.ts`, `src/doctor.ts`, `src/logs.ts`,
  `src/api.ts`, `src/sync-ws.ts` (headless `/ws/coord-sync` firehose consumer for
  `roost api events`), `src/state.ts`.
- **Local loop** — `src/dev.ts`, `src/test.ts`, `src/reset.ts`, `src/cutover.ts`.
- **Server modes** — `src/coord.ts`, `src/worker.ts`, `src/keeper.ts`.

`src/machine-transaction.ts` was relocated here from `apps/shared`: every importer
is in this app (`src/deploy-local.ts`, `src/keeper-refresh.ts`, `src/push.ts`,
`src/quickstart.ts`, `src/windows/windows-relocation-broker.ts`,
`src/windows/windows-update-broker.ts`). It serializes install / update /
relocation / keeper-refresh / deploy against one lock per machine.

## Invariants

- **`src/service-ctl.ts` is the single definer of the service identifiers.**
  `WORKER_UNIT`, `WORKER_AGENT`, `COORD_UNIT`, `COORD_AGENT` are declared once
  here (from the labels in `@roost/shared/paths`) for both POSIX and Windows call
  sites, and it also owns `launchdBootstrapWithRetryCmd`, called from
  `src/deploy.ts`, `src/deploy-local.ts`, and `src/push.ts` — three
  near-identical bootout → retried bootstrap → enable → kickstart sequences that
  had already drifted. Its `XDG` preamble is handed to bash locally and over ssh,
  so keep it byte-for-byte stable. This file is deliberately **not** split along
  the OS boundary: doing so forks those definitions.
- **Release assets are verified in exactly one place.**
  `fetchAndVerifyReleaseAsset` in `src/update.ts` is the only download path — self
  update, Windows fleet preflight, and Windows coordinator update all resolve
  through it, so none can keep a weaker check than its siblings. A published
  `.sha256` sidecar is **required**: it is fetched first, so a 404 or tampered
  sidecar costs no body transfer and nothing is written to `destPath` unverified.
  `ROOST_RELEASE_BASE_URL` is read in exactly one function (`releaseBaseUrl`), so
  the self-updater can be pointed at a mirror; previously only the deploy paths
  honoured it. The Windows CMS `.p7s` signature check layers on top and is
  preserved separately.
- **`console.*` is correct here and only here.** stdout is this app's product
  surface. Coord and worker log through `@roost/shared/log`, and `bun run lint`
  ratchets their `console.*` counts downward. Do not route CLI output through the
  log facade.
- **The Windows brokers cannot be exercised on Linux or macOS.** Their gate is the
  `windows-2022` CI job. A broken dynamic import in `src/main.ts`'s broker handler
  surfaces there, not locally.

## Test

`bun test apps/roost-cli/tests/` — 17 files, 178 tests, all hermetic (Windows
paths are driven through injected fakes). `tests/update.test.ts` pins the release
verification contract; `tests/machine-transaction.test.ts` pins the machine lock.

The repo's own test scripts run through this CLI: `bun run test:unit`,
`bun run test:terminal`, and `bun run test:live-api` all shell into
`roost test <profile>` (`src/test.ts`). `bun run test:worker` is the exception —
it calls `scripts/test-worker.ts` directly, the same script `roost test worker`
and the `unit` profile invoke, because each worker test file needs an isolated
process, keeper PID, and temp root.
