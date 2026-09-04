---
title: "The roost CLI"
description: "Every roost subcommand and every roost api verb, including the headless verbs that drive live panes, and how roost api authorizes itself."
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
| `quickstart` | One-shot local install: Tailscale gate, then coordinator, local worker, and browser pairing |
| `coord` | Run the coordinator (server mode; used by the compiled binary) |
| `worker` | Run the worker (server-side; compiled binary or supervised service) |
| `keeper` | Run the keeper subprocess that hosts this machine's PTYs |
| `update` | Self-update the binary from the latest GitHub release |
| `version` | Print the Roost version |
| `expose <hostname>` | Configure Cloudflare Access browser entry — `--team <team>.cloudflareaccess.com --aud <64-hex> [--config <path>]` |
| `dev` | Start coordinator, worker, and web dev servers |
| `test` | Run all tests in dependency order |
| `deploy <host>` | Deploy a worker to a tailnet host |
| `push` | Push the commit, update the fleet, and kickstart the local coordinator |
| `keeper-refresh <host> --yes` | Re-spawn a host's keeper on current code (destructive) |
| `logs <coord\|worker>` | Tail an app's logs, `--tail N` (default: last 100 lines) |
| `reset` | Nuke local state — database, keys, lock |
| `state` | Print the state snapshot |
| `cutover` | Migrate from the legacy `coordinator.db` to `coordinator_v2.db` |
| `status` | Health readout: Tailscale, coordinator, workers |
| `doctor [--since]` | Anomaly digest from the error logs (default window 24h) |
| `api <verb>` | Headless introspection and control (see below) |
| `join` | Install and register this machine's worker; needs `ROOST_COORDINATOR_URL` and `ROOST_BOOTSTRAP_TOKEN` |
| `add-machine` | Print a one-shot enrollment command — `--platform <macos\|linux\|windows> [--label X] [--publisher-sha256 HEX]` |

`--since` accepts a number plus a unit, so `90m`, `1h`, `24h`, and `7d` are all
valid. `roost logs` also warns when a log file has grown past 100 MB.

Notes on the destructive ones. `keeper-refresh` requires `--yes` because
re-spawning the keeper ends the PTYs it hosts. `reset` deletes local state
outright. `push` narrows with `--targets=host1,host2` and can retain the existing
web bundle with `--no-web`; see [fleet](/docs/fleet/) for how it proves
convergence and rolls back.

## `roost api`

`roost api` is the headless surface: it introspects and drives a live coordinator
without a browser. It is the same RPC surface the web app uses, which is why it is
useful both for scripting and for reproducing a UI bug from a shell.

### Sessions and terminals

| Verb | Arguments |
|---|---|
| `sessions` | — lists every session |
| `spawn` | `<workerFp> <folder>` |
| `kill` | `<sessionId>` |
| `input` | `<sessionId> <text>` — `\n`, `\t`, `\r` escapes are expanded |
| `cells` | `<sessionId>` — structured scrollback rows |
| `events` | `<sessionId> [--secs N]` — live wire-delta monitor, default 5 s |
| `rename` | `<sessionId> [title…]` — an empty title clears the override |
| `assign` | `<sessionId> <workspaceId\|-->` — `--` clears the assignment |
| `attach` | upload local files into a session and print each absolute path |

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
redeems it through the normal browser-redemption RPC. A fresh remote or managed
CLI instead stops with explicit pairing-required guidance. Loopback and tailnet
addresses are not credentials and never authorize the key by themselves.

## Two verbs that were removed

`cat` and `watch` are retired and print an explicit message rather than failing
oddly: use `cells` for scrollback and `events` for a live output stream.

## Next

- [Fleet](/docs/fleet/) — `push`, `deploy`, and coordinator relocation
- [Networking](/docs/networking/) — `expose` and the public deny list
- [Security](/docs/security/) — keys, pairing, revocation, audit
- [Quickstart](/docs/quickstart/) — `quickstart`, `add-machine`, `status`, `doctor`
