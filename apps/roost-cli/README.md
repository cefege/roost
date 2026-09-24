<!-- AUDIENCE: claude -->
<!-- CLI map: src/ owns operator workflows; src/windows/ is the paused Windows adapter. -->
<!-- Protocol meaning is authoritative under protocol/spec; this README records operator ownership and CLI-only policy. -->

# @roost/cli — operator command surface

`roost` is the single install, deploy, update, inspect, and test surface. Run source mode with `bun run roost <sub> [args]` or `bun apps/roost-cli/src/main.ts <sub>`; compiled binaries use the same CLI for `roost coord`, `roost worker`, and `roost keeper` server modes. `src/main.ts::SUBCOMMANDS` is the command registry: a command exists exactly when it has a registry key.

The CLI reaches coord/worker through their package exports and uses `@roost/host` for host runtime seams. It does not import application internals by relative path. Protocol contract index: [`protocol/README.md`](../../protocol/README.md). RPC and transport limits/state machines are normative in [`protocol/spec/coordinator-rpc.md`](../../protocol/spec/coordinator-rpc.md), [`protocol/spec/sync.md`](../../protocol/spec/sync.md), [`protocol/spec/worker-link.md`](../../protocol/spec/worker-link.md), [`protocol/spec/direct-terminal.md`](../../protocol/spec/direct-terminal.md), [`protocol/spec/attachments.md`](../../protocol/spec/attachments.md), and [`protocol/spec/auth-and-pairing.md`](../../protocol/spec/auth-and-pairing.md).

## Entry point

`src/main.ts` dispatches `SUBCOMMANDS`; `--version` aliases `version`, and an unknown command prints usage and exits nonzero. Internal self-exec keys `keeper`, `__keeper-contract`, and `__windows-updater-broker` are omitted from public usage. `src/coord.ts`, `src/worker.ts`, and `src/keeper.ts` are compiled-binary server modes. `src/skill.ts` writes the release-matched skill bytes to stdout and never edits an agent profile.

## Module map

One row per current owned source or test directory.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `apps/roost-cli/src/` | Command dispatch, coordinator/worker/keeper server modes, install/quickstart/join, deploy/push/release/update, service control, status/doctor/logs, headless API, UI-state CLI adapters, reset/cutover, machine transaction, and local test/dev loops. | Protocol schema definitions, coordinator/worker runtime internals, or browser UI state. |
| `apps/roost-cli/src/windows/` | Paused Windows update broker/runtime/journal/rollback/assets, Windows service definitions/S/security/manager/SCM, path/identity safety, and release manifest. | POSIX service policy, worker PTY implementation, or a claim that Windows is currently supported. |
| `apps/roost-cli/tests/` | Bun suites and fixtures for command dispatch, API output, deploy/push/admission, installation, status, and Windows adapter seams. | Production operator state or coordinator/worker process implementation. |

## Command ownership

- **Install and service control:** `src/quickstart*.ts`, `src/join.ts`, `src/add-machine.ts`, `src/install-binary-agents.ts`, `src/service-ctl.ts`, and `src/service-posix.ts` own local provisioning, enrollment, service identifiers, and launchd/systemd command construction.
- **Deploy, push, and keeper refresh:** `src/deploy*.ts`, `src/push*.ts`, `src/direct-keeper-update.ts`, `src/keeper-admission-staging.ts`, and `src/keeper-refresh.ts` own authenticated rollout, journal/recovery, convergence, and explicit destructive keeper maintenance. The CLI obtains keeper admission from the coordinator; it does not own the coordinator link transport.
- **Release and skill:** `src/update.ts`, `src/version.ts`, `src/skill.ts`, and generated skill embed assets own self-update verification and release-matched operator text. `fetchAndVerifyReleaseAsset()` is the one release download verification path.
- **Diagnostics:** `src/status*.ts`, `src/doctor.ts`, `src/logs.ts`, `src/sync-ws.ts`, and `src/state.ts` own health probing, anomaly digest, log output, optional Sync firehose, and state snapshots.
- **Headless API:** `src/api.ts`, `src/api-agent-status.ts`, `src/api-agent-prompt.ts`, `src/api-ui.ts`, `src/api-ui-legacy.ts`, `src/api-terminal-bridge.ts`, and `src/api-command-registry.ts` own authenticated CLI dispatch, exact occupant/prompt fencing, strict UI layout input, bounded terminal-safe output, and command metadata.
- **Local loop:** `src/dev.ts`, `src/test.ts`, `src/reset.ts`, and `src/cutover.ts` own the repository development/test/reset/migration loops. `src/machine-transaction.ts` is the one machine lock used by install/update/deploy/keeper workflows.

## Invariants

- `src/service-posix.ts` is the single POSIX service-definition owner. `src/service-ctl.ts` is the stable facade; identifiers and launchd/systemd ordering must not be forked by OS or caller.
- Keeper update admission is fail closed. A registered worker without current runtime proof cannot be mutated by `push`, deploy, or self-update. Forward-migration state is not treated as rollback-safe.
- Release assets are verified in exactly one place, with a required SHA-256 sidecar fetched before body transfer. Mirror URL policy is centralized.
- `console.*` is allowed here because stdout is the CLI product surface. Coord and worker use `@roost/observability/log` instead.
- A single `machine-transaction` lock serializes install, update, keeper refresh, and deploy. Never add an unlocked second deployment path.
- The paused Windows broker's argv and native helper identity check are a contract. It dynamically imports Windows-only modules and refuses non-Windows execution; Linux/macOS CI cannot prove it.
- The CLI may issue authenticated requests and report outcomes, but terminal input, direct carrier bytes, worker control, and coordinator authorization remain owned by their protocol implementations.

## Test

`bun test apps/roost-cli/tests/` runs the suites with injected platform fakes. The repository gates call this CLI through `roost test unit|worker|terminal|upgrade`; `live-api` is an optional monitor requiring `ROOST_COORD_URL`.
