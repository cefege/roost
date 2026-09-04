---
title: "Fleet: coordinator, workers, keepers"
description: "How Roost splits work between a coordinator, outbound-only workers, and a keeper subprocess — plus per-machine metrics, fleet updates, and relocation."
order: 3
section: "Concepts"
---

## Three roles

**The coordinator** is one Bun process on one machine. It is control plane only:
authentication, an append-only event log, the `sessions` projection folded from
that log, and fan-out to every open browser. It never holds PTY state of its own.

**A worker** runs once per machine and is purely outbound. It dials the
coordinator and owns every shell PTY on its host. The machine running the
coordinator can also run a worker — usually does.

**A keeper** is a subprocess of the worker. One keeper hosts every PTY on that
machine over a single capability-authenticated local endpoint, with a channel id
per session. Because it is a separate process, PTYs survive the worker
restarting: the restarted worker reattaches over the same endpoint and re-adopts
its open sessions.

## Workers dial out, never in

The worker connects to `/ws/coord-worker/:fp` and speaks protobuf over a raw
WebSocket. The direction is always worker to coordinator, so a worker machine
never has to expose an inbound port, open a firewall hole, or hold a
publicly-reachable name. When the connection drops the worker reconnects with
backoff from 500 ms to 30 s while its PTYs keep running in the keeper, then emits
a snapshot on reconnect to reconcile.

## Why the browser and the server agree

Session state is never shipped as a snapshot that can drift. The worker emits
small events — opened, attached, cwd changed, closed. The coordinator appends
each one to the `events` table and folds it into the `sessions` projection inside
a single SQLite transaction, then — strictly after commit — installs that event's
authenticated worker and channel binding and only then publishes it. No browser
can observe a session before the route its first keystroke needs exists.

The browser folds the same events with the same `foldEvent` function from the
shared wire package. Server projection and browser view therefore agree by
construction rather than by careful hand-mirroring. A reconnecting browser sends
the last event id it saw and receives exactly the events it missed.

## Per-machine metrics

Each worker heartbeat carries a host sample: CPU percent, memory used and total,
disk used and total, network receive and transmit bytes per second, and the
timestamp the sample was taken. Sampling is implemented for the supported macOS
and Linux hosts. **Settings → Machines** renders these as a tile per metric, so
"which box has capacity right now" is a glance rather than an SSH session. In
the sidebar, live sessions are grouped by machine.

## What survives what

- **Worker loses the coordinator.** It reconnects with backoff; PTYs keep running
  in the keeper; on reconnect it emits a snapshot to reconcile.
- **Worker process crashes.** The keeper is a separate process, so PTYs survive.
  The restarted worker reattaches and re-adopts open sessions.
- **Browser disconnects.** The sync WebSocket redials on capped backoff (1 s to
  30 s) and backfills missed events from the last event id. The delay is capped;
  the attempt count is not. Only a hidden document sleeps, and one coalesced
  lifecycle wake re-dials in place, so recovery never needs a page reload.
- **Coordinator restarts.** Workers redial, browsers reconnect, and every session
  is re-projected from the event log. Nothing is lost, because the log is the
  source of truth.

## Updating the whole fleet

From a clean checkout at the commit you pushed:

```sh
bun apps/roost-cli/src/main.ts push
```

`roost push` is one journaled transaction across the local POSIX coordinator
and the exact complete registered macOS/Linux worker fleet. It requires at least
one registered worker, a clean complete Git commit, and proof that the commit is
on the configured upstream unless `--no-git` was explicitly chosen.

A registered Windows worker blocks the rollout. Windows host updates are paused
in `v0.5.0`, which publishes no Windows package, manifest, or updater payload;
`push` and `deploy` cannot update one to this release.

Convergence is proven, not assumed. The command snapshots the live coordinator
database, activates and proves the target coordinator in a held state, then
stages and proves every worker at the same SHA with a current keeper and fresh
heartbeat. Only then does it record the durable finalization decision. Before
that decision, any participant failure rolls every worker back and restores and
proves the prior coordinator and database. After it, interrupted recovery can
only finish the target release.

`--targets` may name the exact complete registered worker set, but it cannot
narrow the transaction to a partial fleet. `--no-web` retains the coordinator's
existing web bundle instead of shipping a new one.

One-host POSIX deployment remains a separate source operation:
`bun apps/roost-cli/src/main.ts deploy <host>` stages the exact pushed commit
over SSH and deliberately refuses to run from the standalone release binary,
which contains no Git checkout.

## Coordinator database backups

The coordinator writes a verified SQLite snapshot before it applies pending
migrations to an existing database, and again on a 24-hour interval. Each
snapshot is integrity-checked as a standalone database before compression, and
the 14 newest `coord_v2.<timestamp>.db.gz` archives are retained in a `backups/`
directory beside the database file.

Treat these as same-host rollback material. They do not survive the loss of the
coordinator's disk; copy them somewhere with an independent failure domain if
host-loss recovery matters.

## Moving the coordinator to another machine

Relocation is a two-step, non-destructive-first flow driven from the CLI:

```sh
roost api move-preflight <fp|prefix|label>
roost api move-start     <fp|prefix|label>
roost api move-status    <handoff-id>
```

`move-preflight` sends a check-only request to the target, which validates disk
space, writable directories, its tailnet name, and the absence of an already
active coordinator. It changes nothing and is safe to run against a live cluster.
`move-start` is destructive and re-runs the full preflight server-side, so an
ineligible target fails there rather than half-moving. `move-status` reports the
phase and the source URL for a given handoff.

## Next

- [Networking](/docs/networking/) — the supported topology and the optional public path
- [The terminal](/docs/terminal/) — the data plane between worker and browser
- [The CLI](/docs/cli/) — `push`, `deploy`, `status`, `doctor`, `api`
- [Alternatives](/alternatives/) — how a fleet differs from a single-machine tool
