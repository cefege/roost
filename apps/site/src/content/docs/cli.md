---
title: "The roost CLI"
description: "Every roost subcommand, the release-matched agent skill, every roost api verb, and how roost api authorizes itself."
order: 8
section: "Reference"
---

## Invoking it

```sh
roost <subcommand> [args]
```

`roost --version` and `roost -v` are aliases for `roost version`. An unknown or
missing subcommand prints the usage list and exits 1.

From a source checkout the same entry point is
`bun apps/roost-cli/src/main.ts <subcommand>` — which is how the fleet-update
commands are documented, since they intentionally refuse to run from the
standalone release binary (it contains no Git checkout).

## Subcommands

| Subcommand | What it does |
|---|---|
| `quickstart` | One-shot local install: automatic Tailscale Serve or explicit-certificate direct HTTPS, then coordinator, local worker, and browser pairing |
| `coord` | Run the coordinator (server mode; used by the compiled binary) |
| `worker` | Run the worker (server-side; compiled binary or supervised service) |
| `keeper` | Run the keeper subprocess that hosts this machine's PTYs |
| `update` | Self-update a supported macOS or Linux binary from the latest GitHub release |
| `version` | Print the Roost version |
| `skill` | Write the exact release-matched ROOST agent skill to stdout; accepts no arguments and performs no installation |
| `expose <hostname>` | Configure Cloudflare Access browser entry — `--team <team>.cloudflareaccess.com --aud <64-hex> [--config <path>]` |
| `dev` | Start coordinator, worker, and web dev servers |
| `test` | Run all tests in dependency order |
| `deploy <host>` | Deploy a macOS or Linux worker from a source checkout |
| `push` | Journaled update of the local coordinator and complete registered macOS/Linux fleet |
| `keeper-refresh <host> --yes` | Re-spawn a host's keeper on current code (destructive) |
| `logs <coord\|worker>` | Tail an app's logs, `--tail N` (default: last 100 lines) |
| `reset` | Nuke local state — database, keys, lock |
| `state` | Print the state snapshot |
| `cutover` | Migrate from the legacy `coordinator.db` to `coordinator_v2.db` |
| `status` | Health readout: selected network/TLS mode, coordinator, workers |
| `doctor [--since]` | Anomaly digest from the error logs (default window 24h) |
| `api <verb>` | Headless introspection and control (see below) |
| `join` | Install and register a macOS or Linux worker; needs `ROOST_COORDINATOR_URL` and `ROOST_BOOTSTRAP_TOKEN` |
| `add-machine` | Print a one-shot macOS or Linux enrollment command for automatic mode — `--platform <macos\|linux> [--label X]` |

`--since` accepts a number plus a unit, so `90m`, `1h`, `24h`, and `7d` are all
valid. `roost logs` also warns when a log file has grown past 100 MB.

Notes on the destructive ones. `keeper-refresh` requires `--yes` because
re-spawning the keeper ends the PTYs it hosts. `reset` deletes local state
outright. `push` may use `--targets` to name the exact complete registered
worker set, but cannot narrow the transaction to a partial fleet; `--no-web`
retains the existing web bundle. See [fleet](/docs/fleet/) for its convergence
proof and rollback behavior.

Windows-specific host options that remain in the CLI are non-actionable in
`v0.5.0`: no Windows package, installer, join script, manifest, or updater
payload is published. Windows host install, enrollment, and update are paused,
and a registered Windows worker blocks `push`. A Windows browser client remains
supported.

## Release-matched agent skill

`roost skill` writes only the canonical `SKILL.md` bytes bundled with that
release. A source invocation reads `skills/roost/SKILL.md` directly; a compiled
binary emits the byte-identical generated text embed. The command accepts no
arguments and never edits agent configuration.

Install or update it manually. For OMP's default user profile:

```sh
mkdir -p "$HOME/.omp/agent/skills/roost"
roost skill > "$HOME/.omp/agent/skills/roost/SKILL.md"
```

Restart OMP afterward so it discovers the file. For one project instead, write
the output to `.omp/skills/roost/SKILL.md`. Updating the Roost binary does not
replace either copy automatically; rerun the redirection when you choose to
update the installed instructions.

## `roost api`

`roost api` is the headless surface: it introspects and drives a live
coordinator without a browser through the authenticated Connect service. It is
useful both for scripting and for reproducing a UI bug from a shell; observed
agent reads, waits, and fenced prompts use the same volatile status hub that
feeds browser Sync.

### Sessions and terminals

| Verb | Arguments |
|---|---|
| `sessions` | — lists every session |
| `spawn` | `<workerFp> <folder>` |
| `kill` | `<sessionId>` |
| `input` | `<sessionId> <text> [--enter]` — raw bytes after `\n`, `\t`, `\r` escape expansion; optional Enter |
| `cells` | `<sessionId>` — structured scrollback rows |
| `events` | `<sessionId> [--secs N]` — live wire-delta monitor, default 5 s |
| `rename` | `<sessionId> [title…]` — an empty title clears the override |
| `assign` | `<sessionId> <workspaceId\|-->` — `--` clears the assignment |
| `attach` | upload local files into a session and print each absolute path |

### Observed agent status and fenced prompts

| Verb | Arguments |
|---|---|
| `agent-status` | `<session> [--json]` — reads one authorized session |
| `agents` | `[--json]` — lists current status rows, sorted by session id |
| `agent-wait` | `<session> --until <comma-states> --timeout <duration>` — waits on the exact current occupant |
| `agent-prompt` | `<session> <text> [--wait --until <comma-states> --timeout <duration>]` — sends one occupant-fenced input |

All four verbs use the CLI identity's selected-dashboard authority. Missing and
foreign sessions share the coordinator's not-found response. The two read
verbs use headered TSV; their `source` column renders an absent legacy source as `legacy`,
so screen and legacy rows visibly retain `promptable=false`.

JSON is an explicit stable projection rather than a protobuf dump. Its exact
keys are `session_id`, `agent_id`, `state`, `message`, `status_epoch`,
`occupant_id`, `source`, `revision`, `completed_revision`, `updated_at`, and
`promptable`; absent message, identity, and source values are `null`, and the
revision/timestamp values are numbers. No PID is exposed. Epoch, occupant, and
source are volatile observation and fencing state only: they do not identify a
conversation or grant agent control. Roost owns no agent process, conversation,
transcript, tool call, or approval model. See
[Agents and status](/docs/agents/) for detection and precedence.

`agent-wait` accepts unique states from `blocked,idle,working` and an integral
`ms`, `s`, or `m` timeout capped at five minutes. It first reads the current
identified occupant, then performs one event-driven RPC. Output is exactly
`matched`, `timed_out`, `occupant_changed`, or `session_closed`; every outcome
except `matched` sets a nonzero exit code. The coordinator never polls or
scrapes terminal output.

`agent-prompt` reads one promptable integration status, pins its exact epoch,
occupant and revision, and calls `SessionsPrompt`. Text is the exact single
argv value: it does **not** expand the backslash escapes accepted by `input`.
It must be nonempty and at most 16,384 UTF-8 bytes. Quote shell whitespace as
usual.

The three wait flags are all-or-none. States must be a nonempty unique
comma-list drawn from `blocked,idle,working`; timeout uses the same integral
`ms`, `s`, or `m` syntax and 1 ms through 5 minute bound as `agent-wait`.
Output is:

```text
input	<accepted|rejected|ambiguous>	<written_bytes>	<reason-or->
wait	<matched|timed_out|occupant_changed|session_closed>
```

The reason is at most 200 characters and `written_bytes` at most 16,397.

The second line appears only when `--wait` was requested and input was accepted
or ambiguous; a definite rejection prints only the input line. Rejected or
ambiguous input, or a non-matched wait, sets a nonzero exit code. Input and wait
outcomes stay separate because an ambiguous PTY write may still be followed by
an observed status transition. The CLI and coordinator never retry it.

This command does not address an agent through a hidden API. The worker accepts
text only if the same integration process is still proved and its exact status
is `idle` or `working`. At the final pre-`beginInput` check, a `blocked`,
replaced, screen-only, closed, expired, or newer-revision target rejects before
any keeper write; failure after admission can instead be ambiguous. The shared
terminal encoder normalizes newlines and, when bracketed paste is active,
strips ESC from the text and wraps it before appending one CR. `input` remains
unfenced raw input with its existing escape expansion and optional `--enter`.

### Workers

| Verb | Arguments |
|---|---|
| `workers` | — lists workers and which are routable |
| `worker-rename` | `<fp\|prefix\|label> <newLabel>` |
| `worker-rm` | `<fp\|prefix\|label>` — deregisters the worker |

Anywhere a worker is named you may pass its full fingerprint, a unique
fingerprint prefix, or its label; an ambiguous match is refused with the list of
candidates rather than guessed.

### Workspaces

| Verb | Arguments |
|---|---|
| `workspaces` | — lists workspaces |
| `ws-create` | `<workerFp> <name> <folderPath>` |
| `ws-update` | `<id> [--name X] [--color Y]` |
| `ws-delete` | `<id>` |
| `ws-set-sessions` | `<id> <sessionId…>` — at least one session id is required |

### Driving the live UI

`ui-state` prints what each connected browser tab reported about its own
visibility. Empty output means no browser is open, so `ui` commands would no-op.

`ui <command>` dispatches a command into the live app: `navigate <path>`,
`place-split <sessionId> <destSessionId> <row|col>`, `select-tab <sessionId>`,
`focus-pane <sessionId>`, `move-tab <sessionId> <destSessionId>`,
`arrange <even|rows|tiled|main-vertical|balance>`, `close-tab <sessionId>`, and
`spotlight <sessionId> [--off]`. Dispatch is fire-and-forget: the reported
`delivered` count is the number of sync subscribers at publish time, not a
per-tab acknowledgement.

### Coordinator relocation

`move-preflight <fp|prefix|label>` is non-destructive and safe against a live
cluster. `move-start <fp|prefix|label>` is destructive and re-runs the full
preflight server-side. `move-status <handoff-id>` reports the phase and source
URL. See [fleet](/docs/fleet/).

### Task rows

`tasks [--state X]`, `task-enqueue <payload_json>`, and `task-cancel <id>` read
and write the coordinator's task rows directly; `task-enqueue` parses the payload
first so a malformed one fails at the CLI instead of becoming an opaque queue row.
Roost ships no first-party runner that dequeues these rows, so treat them as a
durable queue primitive rather than an automation feature.

### Device revocation from the host

```sh
roost api device-revoke-local <fingerprint> --yes
```

This is the escape hatch for having lost every authorized browser. It is
destructive, so `--yes` is mandatory, and it only talks to an
`http://127.0.0.1:<port>` coordinator URL — no credentials in the URL, no path,
no query, no fragment. The port comes from `ROOST_COORDINATOR_BIND` (default
`127.0.0.1:4102`) or an explicit `ROOST_COORD_URL`. Anything else is refused
before a request is made.

## How `roost api` enrolls its key

`roost api` always signs with its path-isolated `~/.roost/cli-key`; it never
borrows a worker credential. After authorization it resolves
`AuthDashboardAccess.selected_dashboard_id` and scopes unary and Sync requests
to that dashboard.

An unknown key can enroll automatically only while the CLI is running on the
coordinator host: the host mints a scoped one-shot browser grant and the CLI
redeems it through the normal browser-redemption RPC. A fresh remote CLI instead
stops with explicit pairing-required guidance. Loopback and tailnet addresses
are not credentials and never authorize the key by themselves.

## Two verbs that were removed

`cat` and `watch` are retired and print an explicit message rather than failing
oddly: use `cells` for scrollback and `events` for a live output stream.

## Next

- [Fleet](/docs/fleet/) — `push`, `deploy`, and coordinator relocation
- [Networking](/docs/networking/) — `expose` and the public deny list
- [Security](/docs/security/) — keys, pairing, revocation, audit
- [Quickstart](/docs/quickstart/) — `quickstart`, `add-machine`, `status`, `doctor`
