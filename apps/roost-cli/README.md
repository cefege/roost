# @roost/cli — the one CLI

`roost` is the single operator surface: install, deploy, update, inspect, test.
It replaced 7+ scattered shell scripts. Run it as `bun run roost <sub> [args]`
(root script), `bun apps/roost-cli/src/main.ts <sub>`, or as the compiled
`roost` binary — the compiled binary also uses this CLI for its *server* modes
(`roost coord`, `roost worker`, `roost keeper`).

Path references are relative to `apps/roost-cli/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Entry point

`src/main.ts` holds the `SUBCOMMANDS` dispatch object. It is the source of truth
for the command surface: `src/main.ts` looks the argv up in that object, so a command
exists exactly when it has a key there. `--version` / `-v` alias to `version`; an
unknown key prints `usage()` and exits 1.

`SUBCOMMANDS` has **29 keys**: `usage()` prints 23; the six internal
self-exec/service entries `keeper`, `__keeper-contract`,
`__windows-updater-broker`, `__saas-instance`, `__saas-auth`, and
`__saas-provisioner` are omitted.

| Command | Purpose |
| --- | --- |
| `quickstart` | One-shot local install for direct HTTPS (`--coordinator-url` + `--tls-cert` + `--tls-key`) or Tailscale: validate endpoint group → build SPA → configure POSIX Serve when needed → install coord → deploy local worker → health → open an already-authorized browser via a self-minted `#pair` token |
| `coord` | Run the coordinator in this process (compiled-binary server mode). Lazily `import()`ed so the generated SPA embed never loads into `roost test` |
| `saas <command>` | Root-only managed operator surface: account create/resend/disable/enable/list, reconcile, resolver, encrypted backup, immutable-image rollout, signup credential init |
| `__saas-instance <command>` | **Internal.** Managed-container seed/activation/status/health self-exec surface |
| `__saas-auth serve` | **Internal.** Central auth gateway service |
| `__saas-provisioner serve` | **Internal.** Root-only authenticated provisioning service |
| `worker` | Run the worker in this process (compiled-binary worker mode); same entry the LaunchAgent/unit uses |
| `keeper <sock>` | Run the multiplexed keeper in this process. Internal self-exec target: the worker spawns `roost keeper <sock>` when it is not running under bun |
| `update` | Self-update the binary from the latest GitHub release |
| `__windows-updater-broker` | **Internal, win32-only.** See below — its name and argv are a contract |
| `version` | Print the version / build identity (`--version`, `-v`) |
| `skill` | Write the canonical release-matched `skills/roost/SKILL.md` bytes to stdout; accepts no arguments and never installs or edits agent configuration |
| `expose <hostname>` | Put the coordinator behind Cloudflare Access (`--team`, `--aud`, `--config`) |
| `dev` | Boot coord (:4102) + outbound-only worker + web dev server (:5174) in parallel |
| `test [profile]` | Canonical entry point: `unit`, `worker`, `terminal`, `managed` qualification, `live-api` optional monitor, or `all` |
| `deploy <host> [--force-live]` | Refresh the worker on a tailnet host (macOS rsync + LaunchAgent, Linux in-place checkout). Staging requires keeper update admission from the coordinator registry, except for a worker that reports no keeper runtime at all — that one bootstraps without a journaled keeper update and says so. `--force-live` additionally authorizes the deployed worker to DESTROY every PTY held by a keeper it can neither adopt nor prove empty (a keeper predating binding proof); every shell, dev server, and test in those PTYs exits. It applies to that one deploy and the next deploy clears it |
| `push` | Publish one clean commit, deploy every registered worker, update the coordinator's own checkout, and prove every process reports that commit before returning success |
| `keeper-refresh <host> --yes [--force-live]` | Re-spawn a host's keeper on current code through the coordinator-fenced maintenance RPC. Destructive, explicitly confirmed, and the only workflow authorized to stop a keeper while the worker is live; `--force-live` ends every PTY that keeper hosts. A keeper the worker cannot identify is refused here — retire it with `roost deploy <host> --force-live` instead |
| `logs <coord\|worker> [--tail N]` | Tail an app's log files; warns past 100 MB |
| `reset` | Stop both services, wipe the coord DB + pinned keys + lock, re-run `bun install` |
| `state` | Print a `STATE.md` snapshot to stdout |
| `cutover` | Migrate `coordinator.db` → `coordinator_v2.db` |
| `status` | ✓/✗ health readout: configured endpoint/TLS mode, conditional Tailscale state, both services, coordinator liveness, workers; each failing line carries its remedy |
| `doctor [--since 24h]` | Anomaly digest from the low-volume Tier-1 channel (`main.err.log` + rotated `.N.gz`) |
| `api <verb>` | Dashboard-authorized headless introspection/control over coordinator RPCs: `sessions`, `agent-status`, `agent-wait`, `agent-prompt`, `agents`, `cells`, `input`, `rename`, `assign`, `attach`, `spawn`, `kill`, `workers`, `workspaces`, `ws-*`, `tasks`, `task-*`, `ui`, `ui-state`, `events` |
| `join` | Install + register this machine's worker from a one-shot bootstrap token (driven by the repo-root `join.sh`; needs `ROOST_COORDINATOR_URL` + `ROOST_BOOTSTRAP_TOKEN`) |
| `add-machine --platform <macos\|linux\|windows>` | Mint one worker token and print the platform-specific enrollment command. Coordinator-only |
| `organizations bootstrap-owner` | Managed-only atomic initial owner/organization/dashboard bootstrap; password accepted only through stdin or `ROOST_OWNER_BOOTSTRAP_PASSWORD` |

v0.5.0 releases and deploys self-hosted Roost on macOS/Linux. Both direct
HTTPS and Tailscale endpoint modes are supported. Managed per-account isolation
is qualified but not publicly launched: accounts are operator-created, while
open signup and production managed image publication are off. Windows remains
paused.

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
- **Deploy / push** — `src/deploy.ts` routes first through
  `src/deploy-windows-channel.ts`, then self-host, Linux, or macOS.
  `src/deploy-exec.ts` owns ssh/spawn; `src/deploy-worker-environment.ts` and
  `src/worker-deploy-rollout.ts` own worker contracts;
  `src/deploy-workspaces.ts` expands slim macOS staging and
  `src/deploy-plist-env.ts` parses launchd environment. Platform activation and
  recovery live in `src/deploy-macos-rollout.ts`,
  `src/deploy-linux-recovery.ts`, and `src/deploy-local-activation.ts`.
  `src/push.ts` is the operator entry; `src/push-fleet-rollout.ts` owns atomic
  fleet convergence, `src/push-coordinator.ts` owns the held local target, and
  `src/local-worker-rollout-coordinator.ts` validates intentional journal
  overlap. POSIX journals are `src/posix-deploy-journal.ts`,
  `src/deploy-macos-journal.ts`, `src/deploy-macos-journal-controller.ts`,
  `src/macos-deploy-journal-program.ts`, `src/linux-deploy-journal.ts`,
  `src/linux-deploy-journal-commands.ts`, and
  `src/local-worker-deploy-journal.ts`. Coordinator rollout is split across
  `src/coordinator-deploy-journal.ts`, `src/coordinator-deploy-recovery.ts`,
  `src/coordinator-deploy-finalization.ts`,
  `src/coordinator-deploy-snapshot.ts`,
  `src/coordinator-deploy-release.ts`, and
  `src/coordinator-service-definition.ts`.
  `src/remote-deploy-lock-program.ts` owns remote leases;
  `src/deploy-self-host.ts` is detection only;
  `src/keeper-refresh.ts` owns the explicit destructive workflow.
  `src/direct-keeper-update.ts` resolves keeper update admission from the
  installed coordinator database and `src/keeper-admission-staging.ts` collapses
  its outcome into what a platform driver may stage: a proven update, a
  first-install target, a stale row that still defers to the installed-service
  probe, or the bootstrap allowance for a worker that predates keeper-runtime
  reporting.
- **Windows-only, paused for v0.5.0** —
  `src/windows/windows-update-broker.ts`,
  `src/windows/windows-update-journal.ts`,
  `src/windows/windows-update-control.ts`,
  `src/windows/windows-update-runtime.ts`, plus
  `src/windows/windows-identity.ts`, `src/windows/windows-path-safety.ts`,
  `src/windows/windows-journal-validate.ts`,
  `src/windows/windows-release-manifest.ts`,
  `src/windows/windows-update-assets.ts`,
  `src/windows/windows-update-stable-artifacts.ts`, and
  `src/windows/windows-update-rollback.ts`. Relocation is
  `src/windows/windows-relocation-broker.ts`,
  `src/windows/windows-relocation-control.ts`,
  `src/windows/windows-relocation-journal.ts`. Service splits are
  `src/windows/windows-service-types.ts`,
  `src/windows/windows-service-definitions.ts`,
  `src/windows/windows-service-scm.ts`,
  `src/windows/windows-service-security.ts`, and
  `src/windows/windows-service-manager.ts`.
- **Install + service control** — `src/service-ctl.ts` is the stable
  POSIX/Windows facade; `src/service-posix.ts` owns POSIX identifiers and
  launchd/systemd command construction. Install/enrollment owners are
  `src/install-binary-agents.ts`, `src/machine-transaction.ts`, `src/join.ts`,
  `src/add-machine.ts`, `src/quickstart.ts`, `src/quickstart-runtime.ts`,
  `src/quickstart-endpoint.ts`, `src/quickstart-bootstrap-tokens.ts`, and
  `src/expose.ts`. Managed bootstrap is `src/organizations.ts` +
  `src/organizations-bootstrap-database.ts`.
- **Managed host/runtime** — `src/saas/entry-admission.ts` gates exact Linux
  identities; `src/saas/index.ts` composes the operator command.
  `src/saas/host.ts` is the facade over `src/saas/host-config.ts`,
  `src/saas/host-prerequisites.ts`, and
  `src/saas/host-prerequisite-checks.ts`. `src/saas/layout.ts`,
  `src/saas/docker.ts` + `src/saas/docker-container-contract.ts`,
  `src/saas/caddy.ts`, `src/saas/probe.ts`, `src/saas/backup.ts`, and
  `src/saas/rollout.ts` own the remaining host boundaries.
- **Managed lifecycle/registry** — `src/saas/lifecycle.ts` is the facade over
  `src/saas/lifecycle-contract.ts`, `src/saas/lifecycle-core.ts`,
  `src/saas/lifecycle-account-operations.ts`, and
  `src/saas/lifecycle-reconciliation.ts`. `src/saas/registry.ts` fronts
  `src/saas/registry-model.ts`, `src/saas/registry-row-types.ts`,
  `src/saas/registry-row-mappers.ts`, `src/saas/registry-validation.ts`,
  `src/saas/registry-schema.ts`, `src/saas/registry-storage.ts`,
  `src/saas/registry-coordinator-store.ts`,
  `src/saas/registry-reservation-store.ts`,
  `src/saas/registry-provisioning-job-store.ts`,
  `src/saas/registry-link-ticket-store.ts`, and
  `src/saas/registry-lease-store.ts`. `src/saas/resolver.ts` composes
  `src/saas/resolver-contract.ts` and `src/saas/resolver-request.ts`.
- **Managed provisioning** — `src/saas/provisioner-worker.ts` is the facade
  over `src/saas/provisioning-contract.ts`,
  `src/saas/provisioning-job-loop.ts`,
  `src/saas/provisioning-submission-worker.ts`,
  `src/saas/provisioning-link-ticket.ts`, and
  `src/saas/provisioner-operation.ts`; `src/saas-provisioner-worker.ts` is its
  top-level compatibility re-export.
- **Managed instance** — `src/saas-instance.ts` dispatches through
  `src/saas-instance-types.ts` and `src/saas-instance-command.ts` to
  `src/saas-instance-seed-database.ts`,
  `src/saas-instance-owner-activation.ts`,
  `src/saas-instance-google-owner.ts`, and
  `src/saas-instance-inspection.ts`.
- **Managed auth** — `src/saas-auth/index.ts` composes
  `src/saas-auth/http-server.ts`, `src/saas-auth/gateway-config.ts`,
  `src/saas-auth/request-security.ts`, and `src/saas-auth/canonical-json.ts`.
  `src/saas-auth/state-store.ts` fronts
  `src/saas-auth/state-store-types.ts`,
  `src/saas-auth/state-store-database.ts`,
  `src/saas-auth/state-store-abuse.ts`, `src/saas-auth/state-store-email.ts`,
  `src/saas-auth/state-store-oauth.ts`, and
  `src/saas-auth/state-store-results.ts`. Provider/browser flows are
  `src/saas-auth/provider-http.ts`,
  `src/saas-auth/google-id-token.ts`, `src/saas-auth/turnstile.ts`,
  `src/saas-auth/email-signup.ts`, `src/saas-auth/google-oauth.ts`,
  `src/saas-auth/result-protocol.ts`, and
  `src/saas-auth/federated-assertion.ts`; signup delivery/setup are
  `src/saas-auth/signup-email-outbox.ts` and `src/saas-auth/signup-init.ts`.
  `src/saas-auth/provisioner-client.ts`, `src/saas-auth/private-ipc.ts`,
  `src/saas-auth/private-ipc-client.ts`, and
  `src/saas-auth/private-ipc-framing.ts` own authenticated root IPC.
- **Managed root provisioner** — `src/saas-provisioner/index.ts`,
  `src/saas-provisioner/runtime.ts`, `src/saas-provisioner/server.ts`, and
  `src/saas-provisioner/replay-store.ts`.
- **Release / skill** — `src/update.ts`, `src/version.ts`; `src/skill.ts`
  chooses the generated text embed in compiled binaries and the canonical
  repository file in source mode.
- **Diagnostics** — `src/status.ts` is the facade over
  `src/status-native-probes.ts`, `src/status-report.ts`,
  `src/status-output.ts`, and `src/status-types.ts`; `src/doctor.ts`,
  `src/logs.ts`, `src/sync-ws.ts` (headless firehose), and `src/state.ts`.
- **Headless API** — `src/api.ts` owns authenticated API dispatch;
  `src/api-ui.ts` owns typed `ui-state`, strict layout-file parsing, and
  acknowledged apply outcome formatting. `src/api-ui-legacy.ts` owns exact
  argv parsing for the eight publication-only UI commands, while
  `src/terminal-safe-text.ts` bounds and escapes untrusted human output.
  `src/api-agent-status.ts` owns stable agent-status reads and exact-occupant
  waits, while `src/api-agent-prompt.ts` owns guarded prompt parsing, status
  pinning, and outcome formatting.
  `src/api-command-registry.ts` composes the canonical metadata used to
  validate the API examples in the bundled skill.
- **Local loop** — `src/dev.ts`, `src/test.ts`, `src/reset.ts`, `src/cutover.ts`.
- **Server modes** — `src/coord.ts`, `src/worker.ts`, `src/keeper.ts`.

### `roost skill`

`roost skill` accepts no arguments and writes only the canonical
`skills/roost/SKILL.md` bytes to stdout. Source mode reads that file directly;
release binaries carry the same text through the generated Bun embed. The
command never installs the skill or changes agent configuration.

Installation is an explicit user action. For OMP's default user profile:

```sh
mkdir -p "$HOME/.omp/agent/skills/roost"
roost skill > "$HOME/.omp/agent/skills/roost/SKILL.md"
```

Restart OMP after installing or replacing the file. A project-local OMP
installation uses `.omp/skills/roost/SKILL.md` instead. Rerun the same manual
redirection after updating Roost when you want the installed instructions to
match the new binary.

### `roost api agent-prompt`

`roost api agent-prompt <session> <text> [--wait --until <states> --timeout
<duration>]` reads the current promptable status, pins its exact epoch,
occupant, and revision, then asks `SessionsPrompt` for one fenced input to that
same shell PTY. `<text>` is the exact single argv value—unlike `api input`, the
CLI does not expand `\n`, `\t`, or `\r` spellings—and must be nonempty and at
most 16,384 UTF-8 bytes.

The wait flags are all-or-none. `<states>` is a unique comma-list drawn from
`idle,working,blocked`; duration is an integral `ms`, `s`, or `m` value from
1 ms through 5 minutes. Output begins
`input<TAB><accepted|rejected|ambiguous><TAB><written_bytes><TAB><reason-or->`;
the reason is at most 200 characters and `written_bytes` at most 16,397. When
`--wait` is set and input is accepted or ambiguous, a second line is
`wait<TAB><matched|timed_out|occupant_changed|session_closed>`; definite
rejection prints only the input line. Rejected or ambiguous input and every
non-matched wait set a nonzero exit code. Neither component retries ambiguous
input or prints/logs the prompt text.

`roost api input <session> <text> [--enter]` remains the unfenced raw-input
surface: its documented backslash expansion and optional CR are unchanged.

### `roost api ui`

`roost api ui-state [--json]` reads each reporting browser tab's ephemeral
active path. While that path resolves to an open folder session, the report
also carries a nonempty browser-owned folder key and typed portable
`layout_document`. Human output prints a pane tree only when that document is
present; JSON uses the strict snake_case `LayoutDocumentV1` or `null` off
folder routes, never runtime pane/split IDs or an embedded layout string.
Human output visibly escapes terminal controls and Unicode bidi/format controls
in every remote text field, truncating each at 256 rendered code points or 512
UTF-8 bytes with an explicit marker. `--json` leaves string values lossless and
untruncated.
Reports remain discoverable for a five-minute TTL, so an entry is not proof
that its tab still owns a live writable Sync socket.

The eight interactive commands (`navigate`, `place-split`, `select-tab`,
`focus-pane`, `move-tab`, `arrange`, `close-tab`, and `spotlight`) remain
fire-and-forget. Each prints exactly `delivered=N`, where `N` is the selected
dashboard's Sync-subscriber count when the coordinator publishes—not the
number of tabs that execute or acknowledge it. `--tab <id>` may appear anywhere
after the command name; it filters execution in browsers but does not change
that count, and omitting it broadcasts. Duplicate or unknown options and the
wrong positional arity are errors.

Acknowledged replacement of one live tab's layout is:

```sh
roost api ui apply-layout <file> --tab <id>
```

The file must be strict V1 layout JSON and `--tab` is mandatory and nonempty.
The CLI first reads the retained UI-state projection. No matching tab prints
`target_gone`; the same tab ID under multiple fingerprints prints `rejected`;
neither path publishes an apply. One match pins its fingerprint/tab tuple into
at most one apply RPC, so a browser that later reuses the tab ID cannot take
over the request.

Stdout is exactly one of `applied`, `rejected`, or `target_gone`. `rejected`
and `target_gone` set a nonzero exit code. A stable sanitized reason, when
present, is written to stderr. `applied` proves a commit after the browser's
navigation attempt, not successful navigation completion. The apply RPC is
never retried. `target_gone` means the exact fingerprint/tab/socket
acknowledgement became unavailable through absence, close, replacement, or
timeout; it is not proof that the browser did not commit the layout.

`src/machine-transaction.ts` serializes install/update/relocation/
keeper-refresh/deploy against one lock per machine. Importers are
`src/deploy-local.ts`, `src/keeper-refresh.ts`, `src/push-coordinator.ts`,
`src/quickstart-windows-install.ts`, `src/windows/windows-relocation-broker.ts`,
and `src/windows/windows-update-broker.ts`.

## Invariants

- **`src/service-posix.ts` is the single POSIX service-definition owner.**
  `WORKER_UNIT`, `WORKER_AGENT`, `COORD_UNIT`, `COORD_AGENT`, the XDG preamble,
  and `launchdBootstrapWithRetryCmd` live there; `src/service-ctl.ts` re-exports
  the stable POSIX and Windows service surface. The launchd retry helper reaches
  `src/coordinator-service-definition.ts`, `src/deploy-local.ts`, and
  `src/deploy-macos-journal-controller.ts` through that facade. Do not fork
  identifiers or bootout → bootstrap → enable → kickstart ordering by OS/caller.
- **Keeper update admission is fail closed.** `push`, direct POSIX `deploy`,
  and self-update require authenticated runtime proof before they mutate a
  registered worker. A release that predates keeper-runtime reporting cannot
  be live-upgraded through the new orchestrator: drain its PTYs and install the
  reporting release with that release's updater first. Missing legacy database
  columns project as unproven rows so the attempt reports the blocked worker
  with zero coordinator, repository, worker, or keeper mutation.
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
- **Windows brokers cannot be exercised on Linux/macOS.** Their only gate is
  the conditional `windows-2022` CI job, disabled by default while
  `ROOST_WINDOWS_GATE` is off. v0.5.0 makes no Windows-support claim.

## Test

`bun test apps/roost-cli/tests/` runs the `*.test.ts` files with platform
operations driven through injected fakes. `tests/api-agent-status.test.ts`
pins the public JSON and TSV status contracts; `tests/api-agent-wait.test.ts`
pins wait parsing, occupant pinning, outcomes, and exit behavior;
`tests/coordinator-deploy.test.ts` pins atomic fleet rollback; `tests/update.test.ts`
pins release verification; `tests/machine-transaction.test.ts` pins the machine lock.

The repo's test scripts run through this CLI: `bun run test:unit`,
`bun run test:terminal`, `bun run test:managed`, and
`bun run test:live-api` shell into `roost test <profile>` (`src/test.ts`).
`unit`, `worker`, `terminal`, and Linux/root/Docker/Chromium/`age`-backed
`managed` are release gates; `live-api` requires `ROOST_COORD_URL` and remains
an optional production monitor. `bun run test:worker` calls
`scripts/test-worker.ts` directly—the same runner used by `roost test worker`
and `unit`—because each worker test file needs its own process, keeper, and
temporary root.

The managed profile builds one immutable coordinator image and runs
`tests/managed-browser.e2e.test.ts`,
`tests/saas-provisioning.e2e.test.ts`,
`tests/saas-backup-restore.e2e.test.ts`, and
`tests/saas-open-signup.e2e.test.ts` for routed per-account isolation,
browser auth, backup/restore, and feature-gated signup recovery. Qualification
does not enable production signup or publish that image.
