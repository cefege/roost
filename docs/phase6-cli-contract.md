# The `roost` CLI contract

Every subcommand of the v3 `roost` binary: its arguments, its exit codes, its
exact output, and whether that output is read by a machine or by a person.

This document is a contract, not a description. A change to an output shape or
an exit code is a breaking change for whatever was written against it, and the
change has to be made here in the same commit that makes it in the code.

**Status of the sections below.** `deploy` and `keeper-refresh` are implemented
and their exit codes are as written here. Three things are named rather than
implied, because a contract that reads as measured when it was not is worse than
one that admits it: the **observed** exit-code table is not yet measured against a
real target, the FAILURE-INDEX guard mapping further down is derived from the
index and the code rather than from a run, and the SSH dry-run has not happened.
Nothing in this document was weakened to make a gate pass.

**Where the shapes come from, in priority order.**

1. **`docs/FAILURE-INDEX.md`.** 99 entries, 13 of which name the CLI, its flags
   or its printed text. Its literals — the `DeployFailure:` refusals,
   `deploy exit 5` / `deploy exit 7`, the `spa:` line, `roost doctor`'s
   `deploy.failed` anomaly code — are the de-facto output contract and are
   reproduced verbatim.
2. **`crates/roost-cli/tests/status_output_shape.rs` and
   `tests/doctor_digest_shape.rs`.** The executable specs. `roost status` and
   `roost doctor` have whole-document goldens, so "the readout has not moved" is
   a test result rather than a review opinion.
3. **`GETTING_STARTED.md`.** It names what each command is *for*; see
   [Recorded disagreements](#recorded-disagreements) for the two places where
   it and the v2 code did not agree, and which one this port followed.

**`CLAUDE.md` claimed `GETTING_STARTED.md` documents `roost status` and
`roost doctor` with example output. It does not** — the document has no example
output for any command. The claim is recorded here so the next reader does not
go looking for it, and the goldens above are what actually pin the shapes.

---

## The stdout rule, and why this crate is exempt from it

`roost-cli` is the one crate `cargo xtask lint` exempts from the no-stdout
rule, because its stdout is a product rather than a log. The exemption is a
statement about which side of the line each thing is on:

| Destination | What belongs there | Why |
| --- | --- | --- |
| **stdout** | Anything a person runs and reads: a readout, a version token, a document, a command to copy. | It is the answer to the question that was asked. |
| **stderr** | Diagnostics, progress, remedies, and the one machine-readable failure line. | A consumer piping stdout must never have to filter it. |
| **`tracing`** | Anything about what the FLEET did: state transitions, reconnects, signals. | `roost doctor` reads the coord and worker `*.err.log`, and an unstructured line there is invisible to both. |

Consequences that are load-bearing:

- A **server mode** (`coord`, `worker`, `keeper`) prints nothing itself. Its
  stdout and stderr ARE the service's log channels — the service managers point
  them at `main.out.log` and `main.err.log`.
- An **operator command** gets a subscriber with a `warn` floor, so an `info`
  line can never land in the middle of a readout a script is parsing.
- The only JSON on **stdout** is `roost __keeper-contract` (one line, one
  object) and `roost version --build` (one bare token). Everything else a script
  reads is line-oriented text whose exact shape is pinned by a test.

---

## Global behaviour

### Dispatch

- `roost` with no subcommand, or with an unknown one, is a **usage error**:
  clap writes the usage to stderr and exits **2**. It is deliberately not a
  help screen — an operator who typed the wrong word should be told, not handed
  a page they have to read to find out which word was right.
- `roost --version` and `roost -v` are rewritten to `roost version` before
  parsing. clap carries no version flag of its own on purpose: the binary's
  version is `roost-host`'s artifact version, which is `dev` for a source
  checkout, and clap would print the crate version — two answers to one
  question.

### The failure line

Every failure, from every command, prints exactly one line to **stderr**:

```json
{"cmd": "<subcommand>", "error": "<message>"}
```

`cmd` is the subcommand as typed. stdout stays clean, so a partially completed
command's output is never mistaken for a successful one. This is the shape v2
emitted (`apps/roost-cli/src/main.ts:122`) and the shape a wrapper script
parses.

### Exit codes

| Code | Meaning | Raised by |
| --- | --- | --- |
| **0** | The command did what it was asked. | everything |
| **1** | It did not, and nothing more specific is known. | everything; `roost status` when the install is unhealthy |
| **2** | A usage error: a bad flag, a malformed argument, an unknown verb. | clap; `roost doctor --since`; `roost keeper` with no socket |
| **5** | A keeper could not be adopted safely, and the deploy **stopped without touching it**. | `roost deploy` (reserved; see below) |
| **6** | The target has no coordinator URL and no prior install to reuse one from. | `roost deploy` (reserved) |
| **7** | The build identity could not be proved: a dirty tree, an unpushed commit, or an upstream that is not the target. | `roost deploy`, `roost push` (reserved) |
| **8** | A fleet rollout reached its irreversible point and could not be settled. | `roost push` (reserved) |
| **9** | A remote lease or a remote process died mid-deploy. | `roost deploy`, `roost push` (reserved) |

Codes 4–9 are **reserved by this document** for the install/deploy group so that
the meanings cannot drift between `deploy` and `push`. A command that has no
reserved meaning for a failure uses 1.

**Exit 0 vs exit 1 on the two health commands is the contract that matters
most**, because both are used as gates:

- `roost status` exits 0 when **both** local services are loaded, the
  coordinator answers its identity RPC, and a **declared** front door answers. An
  **undeclared** front door is a valid same-origin install and never fails the
  gate. The fleet rows are deliberately **not** part of it: a sleeping laptop is
  deferred, not broken, and a gate that turned red every night would be ignored.
- `roost doctor` exits 0 when the window holds no signal that is not an
  operator-caused capture lifecycle, no `error`-level line, and no
  server-side (`5xx`) audit row. It exits 2 — not 1 — for a malformed
  `--since`, so a cron wrapper can tell "the operator mistyped" from "the window
  is alarming".

---

## `roost status`

The current-state gate. Human-read; the **exit code** is the machine-read part.

```
roost status [--endpoint ORIGIN]
```

| Argument | Meaning |
| --- | --- |
| `--endpoint ORIGIN` | Speak about this front door instead of the one the installed service definition declares. For a machine whose unit names a URL that has since moved. |

Probes, in order: both service managers (`systemctl --user is-active
<label>.service` / `launchctl print gui/<uid>/<label>`, 5 s deadline), the
coordinator's own `AuthCoordIdentity` POST, the declared front door's, a `HEAD /`
against the coordinator's own listener, and a read-only read of the coordinator
database for the worker roster.

### Output

Exactly this shape, with no trailing newline:

```
roost status
  ✓ coordinator service (roost3-coord)
  ✓ worker service (roost3-worker)
  ✓ coord reachable (git b1d1836a)
  ✓ public url https://dash.example.test
  ✓ spa: served (/srv/roost/releases/current/web)
  workers (2):
    ✓ studio — last seen 12s ago · b1d1836a · Up to date
      keeper: pid 4242, epoch 6f1c2d3e-4a5b-4c6d-8e9f-0a1b2c3d4e5f, 2 channel(s), bindings cccccccccccc, reconciled 3s ago
      terminal cores: 1/4 resident, 0 pending, 12 MiB reserved, 0 refused
  open: https://dash.example.test
```

The marks are exactly three things, and the third is not a failure:

- `✓` — checked, and it passed.
- `✗` — checked, and it did not. The line below it is the remedy.
- `-` — **not probed** (only ever the `spa:` line, when this host has no
  coordinator listener to ask).

Variants an operator will meet:

| Situation | Line |
| --- | --- |
| No front door declared | `  ✓ public url: local-only access` and no `open:` line |
| Declared front door silent | `  ✗ public url <url>` + the two-line remedy naming `AuthCoordIdentity` |
| No coordinator listener here | `  - spa: not probed (no coordinator listener on this host)` |
| Dist stamped, `index.html` gone | `  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=<p> has no index.html)` |
| Dist present, nothing served | `  ✗ spa: MISSING (ROOST_WEB_DIST_PATH=<p> exists but the coordinator serves no page)` |
| No dist ever declared | `  ✗ spa: MISSING (no ROOST_WEB_DIST_PATH and no embedded build)` |
| No workers registered | `  ✗ workers: none registered` |
| Worker with no keeper observation | `      keeper: update admission unproven` |
| Worker with no capacity report | `      terminal cores: capacity unavailable` |
| Neither service loaded | `      → roost quickstart` / `      → roost deploy localhost` |
| Coordinator unreachable | `      → check logs: roost logs coord` |

**The three remedy lines were re-pointed at v3 commands.** v2 printed
`bash apps/coord/scripts/install.sh install`, `bun apps/roost-cli/src/main.ts
deploy localhost` and `(bun run --cwd apps/web build)`, all of which name files
a v3 install does not have. The line shapes, marks, field order and spacing are
unchanged; only the remedy text moved, and it is the one intentional divergence
in this command's output.

### The worker row's update position

```
    <mark> <label> — last seen <N>s ago[ (STALE)] · <short-sha> · <update label>
```

The update label is one of exactly five, and the five wordings are pinned in
**three** places, all of which must change together:

1. `crates/roost-cli/src/status/update_state.rs` — the table itself.
2. `apps/roost-cli/tests/status-output.test.ts` (v2) /
   `crates/roost-cli/tests/status_output_shape.rs` (v3) — the executable spec.
3. `apps/site/src/content/docs/fleet.md:100-102` — the user-facing site.

| State | Wording |
| --- | --- |
| No SHA from one side | `Version unknown` |
| Same SHA as the coordinator | `Up to date` |
| A deploy to that host is in flight | `Updating…` |
| Behind, and reachable | `Update available` |
| Behind, and unreachable | `Update pending — offline` |

**String-table trap:** `roost update` (the binary self-update path) also uses the
phrase "up to date" for a completely different concept. The two must never share
a constant, and `Update pending — offline` must not be confused with push's
deferred-fleet summary, which reuses the phrase `update pending` for the same
fleet state from the other direction.

`deployInFlight` is a **CLI-local** value and stays that way: this process cannot
see the coordinator's in-memory deploy jobs, so a deploy already in flight reads
as `Update available` here. It does not move with the classifier.

---

## `roost doctor`

The anomaly digest. Human-read; the **exit code** is the machine-read part.

```
roost doctor [--since WINDOW] [--session SESSION_ID]
```

| Argument | Default | Meaning |
| --- | --- | --- |
| `--since WINDOW` | `24h` | A number and a **mandatory** unit: `s`, `m`, `h`, `d`. A bare `24` is a usage error — an unbounded or default-interpreted window on a command meant to be pasted into a daily review is how a week of logs silently becomes a day. |
| `--session SESSION_ID` | — | One session's timeline instead of the digest. A prefix of the id matches. |

### What it reads

The **low-volume always-on channel only**: `main.err.log` for coord and worker
plus `keeper.err.log`, and their logrotate compressions (`.N.gz`, decompressed
through `gzip -dc`). The `diag()` firehose in `main.out.log` is deliberately not
read by the digest — it is per-keystroke on a busy session, and a digest that
summarised it would be unreadable. `--session` *does* read `main.out.log`,
because a per-session timeline is exactly the question the firehose answers.

It also reads the coordinator's `audit_log`, which v2's doctor did not. The
section reports what the coordinator already recorded — method, path, status,
when — and does **not** re-derive a diagnosis. A 4xx row is listed and does not
set the exit code: an unauthenticated probe, a revoked key and a malformed
request are decisions the coordinator made on purpose, and an operator causes
them daily, including by running `roost status`. A 5xx row does set it, because
that is the coordinator failing rather than refusing.

### Output

```markdown
# roost doctor — last 24h
host=<HOST or local>  sources=coord+worker(local)  cutoff=2026-06-18 20:13

## signals (always-on anomaly channel)
  🔴 event.append_failed            3  resize_storm×2  (2 sids, src:coord/worker)
  ⚠️  rpc.worker_timeout            1  (1 sid, src:coord)

## infra warnings (reconnect / rate-limit / external)
     1  worker-service / reader_failed

## audit (coordinator request log)
     3  500 WorkersHeartbeat /roost.v1.CoordinatorService/WorkersHeartbeat  last 2026-06-19 20:10
     1  401 AuthCoordIdentity /roost.v1.CoordinatorService/AuthCoordIdentity  last 2026-06-19 18:02

## summary
  window:  2026-06-19 06:02 → 2026-06-19 20:13
  signals: 4 (2 kinds)   infra: 1   errors: 0
  audit:   3 call(s) failed server-side
  health:  run `roost status` (services / coord / public url / workers)
  exit:    1 (review above)
```

- `🔴` for a kind in `ERROR_SIGNALS`, `⚠️` for every other kind. That list is a
  **presentation** subset of the real vocabulary
  (`roost_observability::SignalKind::ALL`, 73 kinds): an unlisted kind still
  appears and still sets the exit code, it just wears a ⚠️.
- Signals sort by count descending, ties by kind name. Infra rows sort the same
  way, capped at 15. Audit rows sort the same way, capped at 15. The cap exists
  because a window with a thousand rejected probes has one thing wrong with it,
  and printing a thousand rows buries the three 500s underneath them.
- A source with no log file at all adds
  `note: no err.log for <app> (<dir>) (service never ran on this host?)`. "The
  worker has never run here" and "the worker had nothing to say" are opposite
  facts and the digest says which one it found.
- The four `terminal.capture_*` **lifecycle** signals are printed and excluded
  from the exit code: an operator who turned on terminal debugging must not make
  the nightly gate red every time they use it. `terminal.capture_failed`,
  `terminal.history_conflict` and `terminal.emission_conflict` are **not**
  excluded — those are the anomalies the feature exists to surface.

### `--session`

```
# roost doctor --session s_abc123  (412 events)
  14:02:11.004 worker  SIGNAL   scrollback.gap  sid=s_abc123
  14:02:11.221 worker  session  bytes  n=1024
```

Empty result prints **why** it is empty, because "no events" from a gated
firehose reads as "nothing happened" and sends the next hour of the
investigation in the wrong direction:

```
  no events. The diag firehose is gated — set ROOST_DIAG=1 (worker/coord) +
  localStorage.roostDiag='1' (SPA) and reproduce; signals show without it.
```

`--session` **always exits 0**. It is a lookup, not a gate, and "no events for
this session" is the most useful thing it can say.

---

## `roost coord`, `roost worker`, `roost keeper`

The three server modes. They print nothing; their output is the service log.

```
roost coord [--bind HOST:PORT] [--db PATH]
roost worker [--coordinator-url URL]
roost keeper <SOCKET_PATH>
```

Each resolves and **validates** its own boot configuration from argv plus the
environment, and refuses before anything binds a socket or dials. The flags are
overlaid onto the same `ROOST_*` names `roost-host` already reads, in memory —
never exported, because `roost push` deploys every target in one process and an
exported identity would leak one machine's label into the next machine's
install.

- `roost coord` — `--bind` overrides `ROOST_COORDINATOR_BIND`, `--db` overrides
  `ROOST_COORDINATOR_DB`. Both are validated by `roost-host` against the same
  rules the installer enforces.
- `roost worker` — `--coordinator-url` overrides `ROOST_COORDINATOR_URL`. The
  keeper socket, pid file, key path and log directory are resolved by
  `roost-worker` from the worker data directory.
- `roost keeper <SOCKET_PATH>` — the self-exec target. **In v3 the keeper is a
  separate binary** (`roost-keeper`) shipped apart from `roost`, precisely so a
  coordinator deploy never disturbs a live PTY; this subcommand execs that
  sibling and **exits 2** if it is not there, rather than silently running
  something else. `SOCKET_PATH` is required: a keeper with no socket has nothing
  to serve and no way to be found.

`serve` blocks and **returns**; it never calls `process::exit`. The CLI owns
shutdown and the exit code around it.


---

## `roost version`

```
roost version [--build]
```

One bare token on stdout and nothing else. `--build` prints the build SHA
instead of the version. This output is read by `roost push` and by a deploy's
release proof, and a decorated line would have to be parsed back apart by every
reader. A source checkout prints `dev`.

---

## `roost logs`

```
roost logs <coord|worker> [--tail N]     # -n is the short form, default 100
```

Hands `main.out.log` and `main.err.log` to `tail -F` with inherited stdio.
`-F` rather than `-f`: the service managers rotate by rename, and a
descriptor-following tail sits on the rotated file forever while the live one
goes unwatched.

A file over 100 MB gets a `[roost-logs] …` warning on **stderr** before the
tail starts. A file that does not exist is skipped; if **neither** exists the
command exits **1** and names both paths it looked at.

---

## `roost reset`

```
roost reset [--dry-run]
```

Stops both local services (`systemctl --user stop` on Linux,
`launchctl bootout gui/<uid>/<label>` on macOS — argv, never a shell string,
because a label can come from the environment), then removes the coordinator
database **triad**: the database, `-wal` and `-shm`. All three, because a reset
that leaves the write-ahead log behind leaves a database whose next open replays
a write the operator just asked to discard.

Keys, journals and the worker's own state survive. The data directory comes from
`roost-host`, so `ROOST_COORD_DATA_DIR` decides what is deleted and an isolated
test install resets its own database.

Every path is printed before it is removed. `--dry-run` prints all of it and
removes nothing.

## `roost state`

```
roost state [--repo DIR]     # default: the working directory
```

Prints a STATE.md snapshot: the branch, the last 5 commits, the first 30 lines
of `git status`, and a timestamp. It reads git and nothing else.

The `--repo` flag is new: v2 resolved the repository root from its own source
path (`new URL("../../../", import.meta.url)`), which a compiled binary does not
have.

## `roost skill`

```
roost skill
```

Prints `skills/roost/SKILL.md` **verbatim, with no decoration**, embedded at
compile time with `include_str!`. A compiled binary has no source tree to resolve
the path against, and a command whose output depends on the working directory is
not a command an agent can rely on. The embedding also makes the pairing honest:
a released binary prints the skill that was in the tree at that commit.

## `roost test`

```
roost test [lint|unit|terminal|upgrade|live-api|all]     # default: all
```

Runs real tools with inherited stdio, printing `>> <name>` before each, and
fails on the first non-zero exit. It composes gates; it reimplements none.

| Profile | Runs |
| --- | --- |
| `lint` | `cargo xtask lint` — the size cap, the crate DAG, the stdout rule, the design ratchet |
| `unit` | `cargo test --workspace` |
| `terminal` | the Playwright real-flow tier (still TypeScript until Phase 7) |
| `upgrade` | the only gate that proves an EXISTING install survives a new release |
| `live-api` | an optional monitor; **refuses** without `ROOST_COORD_URL`. Never a merge blocker. |
| `all` | `lint`, `unit`, `terminal`, `upgrade`, in that order |

v2's `worker` profile is **gone**: it existed to isolate a per-file JavaScript
worker suite, and v3 has no such suite.

## `roost __keeper-contract`

```
roost __keeper-contract
```

Hidden from `--help`; exists for the deploy and upgrade paths, which address it
by string. Prints one line of JSON: the keeper ABI **this release ships**,
including the SHA-256 of the `roost-keeper` binary.

The digest is of a **different file from the one running** — `roost-keeper`, not
`roost`. Reading `current_exe()` here would report the wrong program and every
admission decision made from the output would be about the wrong bytes. Exits 1
when no `roost-keeper` binary sits beside this one: a keeper contract describes
a keeper this release does not ship.

---

## Not in the tree yet

These are part of the Phase 6 command list and are **not implemented in this
slice**. They are recorded here so whoever implements them has the v2 contract
to port, and so nobody discovers their absence at runtime. Nothing in the tree
pretends to be one of them: a missing subcommand is a usage error, not a stub.

| Command | v2 arguments | v2 exit codes | Notes |
| --- | --- | --- | --- |
| `roost quickstart` | `--coordinator-url URL`, `--dry-run` | 1, plus the deploy codes it calls | Installs coord + worker, waits for health, prints a status readout, opens a paired browser. Never prints or logs the one-shot grant. |
| `roost push` | none | 1, 2, 5, 7, 8 | One journaled fleet transaction with a single decision boundary; rolls the whole fleet back on any failure before the finalizing checkpoint. |
| `roost add-machine` | `--platform macos\|linux`, `--label` | 1 | Mints a one-shot worker token and prints a copy-pasteable enrollment command. The URL comes from the **installed coordinator definition**, never from derivation. |
| `roost join` | none; needs `ROOST_COORDINATOR_URL` + `ROOST_BOOTSTRAP_TOKEN` | 7 on a dirty tree | Installs and registers this machine's worker. |
| `roost update` | none | 1 | POSIX atomic self-replace. |
| `roost api <verb>` | `sessions`, `agents`, `agent-status <session> [--json]`, `agent-wait <session> --until STATES --timeout DURATION`, `agent-prompt`, `input`, `ui`, `ui-state`, `ws-*`, `tasks`, `cells`, `workers`, `workspaces`, … | 1, 2 | Must be built on the **generated** Connect types, never a hand-rolled method name. |
| `roost dev` | none | 0 | Three dev servers with SIGINT fan-out. |
| `roost cutover` | `--force` | 2, 3 | **Deliberately dropped.** It migrated `coordinator.db` → `coordinator_v2.db`, and v3 is a fresh install that must never open a v2 database. |
| `roost __windows-updater-broker` | none | — | **Deliberately dropped.** v3 ships Linux and macOS only. |


---

## `roost deploy <host>`

Replace one machine's release and restart its worker, over ssh. The only command
in this binary that changes a *different* machine, and the only one where a bug
ends somebody's shells rather than a request.

```
roost deploy <host> [--label L] [--reachable-addr A] [--source-root DIR]
                    [--expected-sha SHA] [--expected-manifest-sha256 HEX]
                    [--coordinator-release] [--force-live]
```

| Argument | Meaning |
| --- | --- |
| `<host>` | The target: a host this machine can ssh to, or `user@host`. |
| `--label L` | The name the target enrolls under. Never taken from this shell's environment for a remote host — see the identity rule below. |
| `--reachable-addr A` | The address the rest of the fleet reaches the target at. Same rule. |
| `--source-root DIR` | The checkout to build the release from. Defaults to the checkout this binary was built from. |
| `--expected-sha SHA` | The build this deploy is required to install: a second, independent check on the build identity. |
| `--expected-manifest-sha256 HEX` | The release digest this deploy is required to install. |
| `--coordinator-release` | Prove the build against the installed coordinator's own release rather than against an upstream tip. What a coordinator-started deploy uses. |
| `--force-live` | Authorize the new worker to **destroy every PTY** a keeper it cannot adopt holds, for this deploy only. |

`--allow-unpublished-local` was removed from the shipped surface: it is
restricted to the localhost quickstart path, which is `roost quickstart`'s flag,
and a deploy that accepts it would let a command that mutates a remote machine
ship a commit that exists nowhere else.

### The order, because the order is the safety property

1. Prove what is being shipped, **before building** (exit 7).
2. Prove the target answers, **before building** (exit 2).
3. Read the target's platform and architecture, and build the release for them
   (exit 3, 4).
4. Read the target's own installed definition for its coordinator URL and its
   identity (exit 6).
5. Ask the coordinator whether the target's keeper may be carried across, and
   act on that answer — **before anything on the target is touched** (exit 5).
6. Take the target's machine transaction, swap the definition, activate, prove
   the service is up, settle, retire the prior release.
7. Prove the keeper converged, against a baseline recorded at step 5.

Steps 5 and 6 are the pair that matters. Every keeper refusal happens before the
definition is replaced, so a machine whose keeper cannot be safely carried
across is left exactly as it was.

### The identity rule

`ROOST_WORKER_LABEL` and `ROOST_REACHABLE_ADDR` name **one machine**, and the
process holding this shell's environment is the box running `roost deploy`, not
the target. A deploy that adopted an ambient value installs the *deploying*
box's label and reachable address on the target: the coordinator then lists two
workers under one name, and because a reachable address is what a browser builds
a machine's location from, the wrong machine is addressable under the right name.
Both values resolved, so the deploy looks complete.

So those two keys resolve from `--label` / `--reachable-addr` or from the
target's own installed definition, and from nowhere else. An ambient export over
a target with no prior install is a **refusal (exit 6) naming the flag**,
because guessing which machine the operator meant is how a fleet gets mislabelled.
Every other `ROOST_*` key keeps its ambient fallback; it describes the fleet, not
one box.

### Exit codes

The 4–9 range is reserved for the install/deploy group precisely so that `deploy`
and `push` cannot drift apart. **5 means a keeper was not adopted in either
command**, and a wrapper that reads 5 from one and 1 from the other is a wrapper
that retries the destruction of somebody's shells.

| Code | Meaning | Raised by |
| --- | --- | --- |
| 1 | The operator's own invocation is wrong: no host, a malformed `--label`, a `--source-root` that is not a path. | `deploy` |
| 2 | The target could not be reached, or answered nothing. **Also** clap's code for a rejected flag, so 2 is both "usage error" and "ssh failed", exactly as in v2. | `deploy`, clap |
| 3 | The target has no runtime that can run the release: not Linux or macOS, or an architecture v3 does not ship. | `deploy` |
| 4 | The release this deploy would ship could not be built here, or its digest is not the one that was required. | `deploy` |
| 5 | A keeper could not be adopted safely, and the deploy **stopped without touching it**. | `deploy` |
| 6 | The target has no coordinator URL and no prior install to reuse one from; or an ambient identity key would have mislabelled the machine. | `deploy` |
| 7 | The build identity could not be proved: a dirty tree, an unpushed commit, or a checkout that is not the release it claims to be. | `deploy` |
| 8 | The definition was replaced and the release could not be settled after that. The target's journal is retained and the next deploy resolves it. | `deploy` |
| 9 | A remote process died, or the target's machine transaction was lost, while the deploy held it. | `deploy` |

### stdout

Progress goes to **stderr**; the one line an operator asked for goes to **stdout**.
A deploy that fails halfway has still changed a machine, and a script that reads
stdout must never be able to mistake a partial run for a settled one.

---

## `roost keeper-refresh <host>`

Shut a machine's keeper down **empty**, leaving its worker installed. Narrower
and more destructive than `deploy`, and the narrowness is the design: it replaces
no release, so a refresh that goes wrong leaves a machine whose worker is still
installed and whose keeper is simply gone. The next deploy starts a fresh empty
keeper and nothing about the failure is permanent.

```
roost keeper-refresh <host> --yes [--force-live]
```

| Argument | Meaning |
| --- | --- |
| `<host>` | The target: a host this machine can ssh to, or `user@host`. |
| `--yes` | **Required.** This command exists to destroy PTYs and an operator should have to say so on the command line. |
| `--force-live` | Authorize the destruction of live PTYs, not merely of an empty keeper. |

The target's machine transaction is held for the whole operation even though
nothing is installed, because a keeper shutdown racing a deploy is the case where
a deploy rolls back to a definition whose keeper no longer exists.

**`--force-live` requires a fresh keeper proof from the coordinator.** Destroying
live channels requires knowing *which* channels, and a stale or absent
observation is not that. The warning names the keeper pid, epoch and channel
count before the destruction is requested.

**Keeper maintenance is coordinator-fenced**: the coordinator drains every
channel-creating command and then asks the authenticated worker to perform one
deliberate shutdown. It cannot be done by asking a machine directly, and a
command that tried would be racing every session the fleet is holding open.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The keeper is down. |
| 1 | The refresh did not happen: the worker is stale, no keeper runtime was proven, the coordinator refused, or no coordinator could be reached. |
| 2 | A usage error: no host, or `--yes` absent. |

This is the only reserved code this command uses, and that is deliberate. The
4–9 range is shared with `deploy` and `push`, and a command with no reserved
meaning for a failure uses 1 — inventing a fifth reading of, say, 9 between two
commands is exactly what the reservation exists to prevent.

---

## The hidden target-side commands

`roost __remote-facts`, `__remote-evidence`, `__remote-transaction` and
`__remote-apply` are **not part of the product surface**. They are hidden from
`--help` and they are the only way a deploy can change a remote machine.

**Why they exist.** Everything that decides *what should happen* to a target runs
on the deploying box: is its build identity proved, may its keeper be carried
across, where does its identity come from. Everything that decides *what the
target's own files say* runs on the target. Each side answers with a value the
other cannot misread, and the boundary between them is a manifest and a report
rather than a shared guess about where a machine keeps its state. A deploying box
that derived the target's release root from its own idea of the target's home
would be right until an operator moved `ROOST_VERSIONS_DIR` — and then it would
install the new release somewhere the retired one never lived.

| Command | Question | Answers with |
| --- | --- | --- |
| `__remote-facts` | what is installed here? | one line of JSON: platform, home, worker label, definition path, release root, service directory, installed program, and every `ROOST_*` the installed definition carries |
| `__remote-evidence` | may a release be staged? | the evidence markers, from one probe command |
| `__remote-transaction --kind deploy\|keeper-refresh` | hold this machine still | a line, then it blocks on stdin until released |
| `__remote-apply` | install the release this manifest names | one line of JSON, the report |

**What the hiding carries.** A deploy addresses these by string over ssh, the
same way it addresses `__keeper-contract`. None of them is something a person
should ever type, and the reasons are specific rather than aesthetic:

- `__remote-apply` **refuses unless a machine transaction is held**, so the only
  way to reach it is through a deploy that took the lock. An operator running it
  by hand would be doing an unsynchronized mutation of a live machine, and the
  command is built to refuse exactly that.
- `__remote-transaction` holds a machine still for an unbounded time. A person
  who found it in a help listing and ran it would wedge every later deploy and
  every keeper refresh on that machine until they noticed.
- `__remote-facts` and `__remote-evidence` are read-only, but they print the
  target's own install layout. A product surface should not advertise a command
  whose output describes another machine's filesystem to whoever can run the
  binary.

If you find one of these in a `--help` listing, that is a bug in this document's
author or in the `hide` attribute, not a new feature.

**`__remote-apply` takes its manifest on stdin and answers on stdout, one line
each.** The manifest is versioned and the target refuses a schema it does not
have rather than reading the fields it recognises. The report is versioned the
same way, and a deploying box refuses a report whose vocabulary it does not
know — a report that parsed into "settled" because a field was unknown is the one
failure this pair of documents exists to prevent.

---

## The FAILURE-INDEX guards behind `roost deploy`

The failure index has no section called "Deployment journals"; that phrase is in
the plan, not the document. The deploy-relevant entries live under **"Worker,
keeper and host"**, with one under **"Product boundaries and process"**. All
thirteen are mapped to the code that satisfies them, because a deploy path with
no named guard behind it is the thing this project has already been bitten by.

There is no literal heading to grep, so this table is the index for it.

| FAILURE-INDEX entry | The code that satisfies it |
| --- | --- |
| Repairing a dead worker demands that the dead worker be running | `deploy/admission.rs`: the refusal is returned as a claim about the **coordinator's registry**, and `deploy/target_evidence.rs` + `deploy/keeper_step.rs::decide` decide it against the **target's own probe**. A stale row over a machine running nothing stages; a running worker, a keeper holding channels, an unreachable service manager and a missing `pgrep` all refuse. |
| …a keeper socket file is not evidence | `target_worker_evidence_command` never tests the socket file. It outlives the keeper that created it, so it can neither prove nor disprove anything the process counts do not. |
| …an unreachable service manager reads like a stopped one | darwin corroborates with `launchctl print-disabled`, a query that succeeds either way; linux needs none, because `systemctl show` already exits 0 for a unit it has never heard of. `status/service_probe.rs::service_state_command`. |
| …`pgrep -f` sees this deploy's own ssh command line | The keeper socket name's first character is bracketed into a character class and its regex metacharacters escaped, so the pattern cannot match the text that carries it. |
| A remote deploy hands the target the deploying box's identity | `deploy/identity_env.rs`: `DEPLOY_IDENTITY_ENV_FLAGS` + `resolve_remote_deploy_identity`. An identity key has no ambient fallback; an ambient export over a fresh target is exit 6 naming the flag. |
| A one-shot deploy flag stops at the installer process | `--force-live` travels in the manifest environment and reaches the definition through the spec's own settings path, while `worker_install_environment` strips `ROOST_BOOTSTRAP_TOKEN` and `ROOST_KEEPER_FORCE_LIVE_RETIRE` from **every** prior install. A definition that still carried the retire authorization would re-authorize destroying live channels on every later restart. |
| A rollback proof no release can satisfy wedges every later deploy | `deploy/apply.rs` calls the already-ported `resolve_interrupted_deploy` **before it writes anything**, and refuses rather than overwriting a retained journal. The roll-forward resolution belongs to `services/deploy_journal.rs`, which already owns it. |
| Settlement retires the prior release with a command only a worktree accepts | `deploy/retire.rs::plan_retirement` asks `git worktree list --porcelain` whether the path is a registered worktree and removes the directory outright when it is not — behind the symlink refusal and the release-root confinement, which are the guards that make a recursive removal safe at all. |
| Quoting a systemd path directive because quoting is "safer" | `deploy/installed.rs::systemd_working_directory` reads `WorkingDirectory=` **raw** and reverses only the writer's own `%%` doubling. It never unquotes: a quoted value is quoted in the unit. |
| …writing a definition is not activating it | `services/deploy_transaction.rs` + `PlatformServiceManager::await_active`, reused rather than reimplemented. A definition that loads but does not start is a failed deploy. |
| A retired release's dist leaves every page a 404 while the API still answers | `identity_env::NEVER_CARRIED_FORWARD` drops `ROOST_WEB_DIST_PATH` (and `ROOST_EXEC_BIN` / `ROOST_WORKDIR`) from every prior install. A worker deploy stamps no dist at all. |
| Moving a keeper-imported file makes every live keeper unadoptable | `admission::direct_keeper_update_admission` delegates to `roost_protocol::keeper_update::keeper_update_admission`, so a digest difference is unadoptable **only while channels exist** and is replaceable the moment the keeper is provably empty. |
| Coordinator-started worker deploys exit 7 from a detached release worktree | `identity::coordinator_release_git_sha_or_die` proves the source checkout **is** the installed coordinator release, at the build the installed definition stamps, instead of asking a detached worktree for a publishable upstream. Selected by `--coordinator-release`. |


---

## Recorded disagreements

Where the v2 code and `GETTING_STARTED.md` did not say the same thing, this is
what was decided and why.

1. **"documented with example output in `GETTING_STARTED.md`" is false.**
   `CLAUDE.md` asserted it for both health commands; the document has no example
   output for any command. The goldens in `tests/status_output_shape.rs` and
   `tests/doctor_digest_shape.rs` are what pin the shapes instead.
2. **`GETTING_STARTED.md` says `roost status` "reports … the configured public
   URL and whether it answers `AuthCoordIdentity`".** True, with one qualifier
   the document omits: an **undeclared** public URL prints as informational and
   does not fail the run, and the liveness probe prefers the coordinator's own
   loopback bind over the front door so a half-wired front door never reads as a
   dead coordinator. The document's own claim — "a public URL that is not
   configured prints as informational" — agrees; the sentence above it does not
   mention the case.
3. **`GETTING_STARTED.md` says the exit status gates "both local services,
   coordinator reachability, and that public-URL answer"** and then says to
   "inspect the fleet rows rather than treating that exit status as proof that
   every remote worker converged". The code agrees exactly: worker rows are
   printed and excluded from the exit code.
4. **`roost status` remedies name v2 paths.** Re-pointed at `roost quickstart`,
   `roost deploy localhost` and `dx build`. See the command section.
5. **`roost version --build` and the fleet "Up to date"** share the phrase "up
   to date" for unrelated things. Kept apart deliberately; see the string-table
   trap above.

## The stdout exemption, restated as a rule

Adding a `println!` to this crate is a decision, not a convenience. Before
adding one, answer: **is this the answer to the question the operator asked?**
If yes, stdout. If it is about what the fleet did, it is a `tracing` event, and
if it is about this command's own progress or a remedy, it is stderr.
