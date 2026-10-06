# The `roost` CLI contract

Every subcommand of the v3 `roost` binary: its arguments, its exit codes, its
exact output, and whether that output is read by a machine or by a person.

This document is a contract, not a description. A change to an output shape or
an exit code is a breaking change for whatever was written against it, and the
change has to be made here in the same commit that makes it in the code.

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

- `roost status` exits 0 when **both** local services are loaded, linger is on
  for the account (Linux), the coordinator answers its identity RPC, and a
  **declared** front door answers. An
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
<label>.service` / `launchctl print gui/<uid>/<label>`, 5 s deadline), linger on
Linux (`id -un`, then `loginctl show-user <user> -p Linger --value`, read only),
the coordinator's own `AuthCoordIdentity` POST, the declared front door's, a
`HEAD /` against the coordinator's own listener, and a read-only read of the
coordinator database for the worker roster.

### Output

Exactly this shape, with no trailing newline:

```
roost status
  ✓ coordinator service (roost3-coord)
  ✓ worker service (roost3-worker)
  ✓ linger on (mike)
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
| Linger off (Linux) | `  ✗ linger off (<user>) — services stop at logout; run: sudo loginctl enable-linger <user>`; fails the gate like a stopped service |
| macOS | no `linger` line |

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

The update label is one of exactly five. Two of the three places that pin the
wordings are in this tree, and both must change together:

1. `crates/roost-protocol/src/fleet_update.rs` — the table itself
   (`worker_update_label`, `worker_update_state`), which `status/render.rs` prints.
2. `apps/roost-cli/tests/status-output.test.ts` (v2) /
   `crates/roost-cli/tests/status_output_shape.rs` (v3) — the executable spec.

The third was the user-facing fleet page on the separate documentation site,
which is not in this repository; its copy is changed with these two.

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
  rules the installer enforces. `ROOST_COORDINATOR_DATABASE_URL` (a
  `postgres://` URL) replaces the SQLite file entirely; setting it together
  with `ROOST_COORDINATOR_DB` (or `--db`) is refused, never ranked.
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

## `roost deploy <host>`

```
roost deploy <host> [--label LABEL] [--reachable-addr ADDR]
                  [--source-root DIR] [--expected-sha SHA]
                  [--expected-manifest-sha256 HEX]
                  [--allow-unpublished-local] [--coordinator-release]
                  [--force-live] [--web-dist DIR] [--release TAG]
```

The one command in this crate that replaces the binary every live PTY on a
machine depends on. Its order is the safety property and is stated once, in
`crates/roost-cli/src/deploy/run.rs`: prove what is being shipped, probe the
target, **ask the coordinator whether the target's keeper may be carried across
BEFORE the target's definition is replaced**, and only then touch the machine.
The keeper question is asked early and acted on early because its answer decides
whether the machine may be touched at all; the definition is replaced last
because that is the only step that is hard to put back on its own.

Probing a Linux target includes linger for the ssh account, before anything is
built or staged: when it is off the deploy runs `loginctl enable-linger <user>`,
then `sudo -n loginctl enable-linger <user>`, re-reads, and exits **3**
(`NO_REMOTE_RUNTIME`) with `<host>: linger is off for <user>: Roost services
stop when you log out. Run: sudo loginctl enable-linger <user>` if it is still
off — a worker installed there would stop at that account's logout.

`--force-live` authorizes the new worker to **destroy every PTY** held by a
keeper it cannot adopt, for that deploy only, and prints a three-line warning
before doing it. It is one-shot on both sides: the definition carries it only
when the flag was given, and the next deploy strips it.

### The guard map

`docs/FAILURE-INDEX.md` records **thirteen** entries whose symptom is a deploy
that misbehaves on a real machine. Each one was a shipped defect, and each one
is a place where being wrong is **silent** — the deploy reports success, or
refuses a machine that was healthy, and nothing raises. There is no literal
"Deployment journals" heading in the index; twelve of them live under **"Worker,
keeper and host"** and one under **"Product boundaries and process"**. This
table is the mapping from each entry to the code that satisfies it and the test
that holds it, so a future reader can tell which lines are load-bearing.

The three entries in that same section that are **not** here are terminal
rendering, session adoption and viewport-resize defects; they belong to the
worker and the browser, and no step of a deploy can produce them.

Paths are relative to the repository root; tests to `crates/roost-cli/tests/`.

| FAILURE-INDEX entry | The code that satisfies it | The guard |
| --- | --- | --- |
| A worker throttled by its own cgroup looks healthy | `crates/roost-cli/src/services/memory_limits.rs` — `ResourceLimits::{coordinator, worker}` derive the ceilings from the host's own `MemTotal` rather than from constants; `services/systemd_unit.rs::render_systemd_unit` emits `MemoryHigh=` always, `MemoryMax=` only for the coordinator, and `OOMPolicy=continue` for the worker | `services_definition_text.rs` — `the_linux_worker_unit_keeps_its_keeper_out_of_the_cgroup_kill` |
| Quoting a systemd path directive because quoting is "safer" | Writer: `services/systemd_unit.rs::render_systemd_unit` via `raw_path_value` — `WorkingDirectory=`, `StandardOutput=`, `StandardError=` are emitted **raw**; `ExecStart=` and `Environment=` stay quoted. Reader: `deploy/installed.rs::systemd_working_directory` reads the raw value and reverses only the writer's own `%%` | `services_definition_text.rs` — `the_linux_coordinator_unit_names_its_binary_its_paths_and_its_limits`, `a_linux_unit_is_accepted_by_systemd_itself_when_the_tool_is_present`; `deploy_installed_release.rs` — `a_working_directory_is_read_raw` |
| A fresh macOS account has no LaunchAgents directory | `services/install.rs::ensure_service_directories` creates the data directory, the log directory **and the definition's own parent**; `deploy/apply.rs::apply` calls it before anything is staged into that parent | `services_install_idempotence.rs` — `the directories a service needs are created before the first definition` |
| A remote deploy hands the target the deploying box's identity | `deploy/identity_env.rs::DEPLOY_IDENTITY_ENV_FLAGS` names the identity keys and the flag that supplies each; `resolve_deploy_env_value` takes an explicit `EnvTarget` and gives an identity key **no ambient fallback at all** for `EnvTarget::Remote`; `resolve_remote_deploy_identity` refuses with exit 6 when the deploying shell exported that key and nothing else resolved it | `deploy_remote_identity.rs` — `an_identity_key_never_resolves_from_the_deploying_shell`, `an_ambient_identity_export_refuses_and_names_the_flag`, `an_unresolvable_identity_with_nothing_exported_is_allowed` |
| Roost cannot upgrade the integration asset Roost installed | **Not in this command's path, and not yet ported.** The asset installer lives in the worker (`agents/{install,manifests}.rs`, Track W slice W7b) and v3 has no agent-integration installer, so a v3 deploy installs no integration asset and cannot produce the symptom. Recorded here so the absence is not read as coverage — the guard has to land with that slice, and it is `hasIntegrationOwnership` accepting the marker as a whitespace-delimited token on **any** `//` line, never a positional window | not yet — see the Track W agents installer |
| A one-shot deploy flag stops at the installer process | `services/service_spec.rs::ServiceSpec::with_decided_one_shots` is the ONE arming site: it copies `ROOST_BOOTSTRAP_TOKEN` and `KEEPER_FORCE_LIVE_RETIRE_ENV` out of the manifest's **decided** environment onto the resolved spec, and `deploy/apply.rs:149` calls it immediately after `ServiceSpec::resolve`. `deploy/identity_env.rs::worker_install_environment` strips both from a prior install, so a grant is armed for exactly one deploy and is not inherited by the next. **This row previously pointed at `worker_install_environment` and `definition_environment` and was wrong**: both are composition sites, and composition was never where the flag died. `ServiceSpec::resolve` refuses one-shot keys because an ambient environment must never arm a credential — that refusal is correct and was the *only* link in the chain, so "a plain resolve refuses to arm a grant" read in practice as "no grant can ever be armed". The gap was the missing link between the refusal and the writer, not a wrong rule at the refusal. | `services_definition_text.rs` — `a_decided_one_shot_reaches_the_rendered_definition` (the load-bearing one: it asserts on the **rendered unit text** in both directions, that a decided grant reaches the bytes a service manager reads and that an install which decided nothing arms nothing); `a_one_shot_grant_is_never_carried_into_a_definition` holds the resolve-side refusal; `deploy_remote_identity.rs` — `a_deploy_never_carries_a_one_shot_grant_forward` holds never-carried-forward. The worker's side of the same entry — spending the grant at activation — is `crates/roost-worker/src/host/install.rs::spend_keeper_force_live_retire_authorization`, called from `runtime/mod.rs:146` in `serve_until` after keeper admission and before the link, guarded by `crates/roost-worker/tests/worker_retire_authorization.rs`. |
| Repairing a dead worker demands that the dead worker be running | `deploy/admission.rs::keeper_admission_staging` returns the coordinator's refusal as a **claim about the registry**, and `deploy/keeper_step.rs::decide` is what decides it — against `deploy/target_evidence.rs::installed_service_verdict`. The one probe is `target_worker_evidence_command`: it corroborates darwin with a `launchctl print-disabled` domain query, refuses when `pgrep` is absent, and a keeper **socket file** decides nothing (it outlives the keeper that made it) | `deploy_keeper_admission.rs` — `a_stale_row_over_a_target_running_nothing_stages`, `a_stale_row_over_a_running_worker_still_refuses`, `a_keeper_holding_channels_refuses_even_with_the_worker_stopped`, `an_unknown_never_stages`, `a_keeper_socket_file_decides_nothing`, `the_darwin_probe_distinguishes_an_unreachable_launchd` |
| A rollback proof no release can satisfy wedges every later deploy | `deploy/apply.rs::apply` calls the already-ported `services::deploy_transaction::resolve_interrupted_deploy` **before it writes anything**, so the definition this deploy replaces is one the machine can actually run | `services_deploy_recovery.rs` — `a_deploy_left_in_flight_is_resolved_before_the_next_one_starts`, `a_journal_whose_shape_this_build_does_not_know_is_refused_rather_than_ignored` |
| Settlement retires the prior release with a command only a worktree accepts | `deploy/retire.rs::plan_retirement` asks git (`registered_worktrees`, `git worktree list --porcelain`) and otherwise removes the directory outright. The release-root confinement and the symlink refusal run **before** the worktree question, because they are what makes a plain recursive removal safe | `deploy_installed_release.rs` — `a_staged_prior_release_is_retired_without_being_a_worktree`, `retirement_is_confined_to_the_release_root` |
| A retired release's dist leaves every page a 404 while the API still answers | `deploy/identity_env.rs::NEVER_CARRIED_FORWARD` drops `ROOST_WEB_DIST_PATH` from every prior install, and `deploy/invocation.rs::definition_environment` never inserts it, so a carried value can never name the release the next settlement deletes. `status/report.rs::SpaStatus` keeps `serves` and `web_dist_present` as separate facts and `status/collect.rs` HEADs the coordinator's own root | `status_output_shape.rs` — the three `spa: MISSING` variants |
| An installer inherits a sibling service's dist path from the shell that ran it | The v2 shell installers do not exist in v3, so the hole they had is closed structurally: a definition is rendered from a `ServiceSpec` by `services/systemd_unit.rs` / `services/launchd_plist.rs`, and the environment it carries is `deploy/apply_release.rs::install_environment` — the target's own `HOME` and `PATH` plus the manifest's **decided** values. There is no route by which an ambient `ROOST_WEB_DIST_PATH` reaches a worker definition | `deploy_remote_identity.rs` — `a_deploy_never_carries_a_one_shot_grant_forward` (the dist key is on the same never-carried list) |
| Moving a keeper-imported file makes every live keeper unadoptable | `deploy/admission.rs::direct_keeper_update_admission` delegates to `roost_protocol::keeper_update::keeper_update_admission`, which compares the implementation digest the **running** keeper reports against the one the release ships. `deploy/release.rs::read_keeper_contract` reads that contract from the **staged bytes** rather than from this process, because in this process `roost-keeper` is whatever release the CLI was built from | `deploy_keeper_classification.rs` — `a_different_keeper_binary_is_unadoptable_only_while_it_holds_channels`, `a_keeper_that_cannot_name_its_binary_is_unproven`, `the_same_keeper_binary_is_preservable` |
| Coordinator-started worker deploys exit 7 from a detached release worktree | `deploy/identity.rs::coordinator_release_git_sha_or_die`, selected by `deploy/invocation.rs::prove_identity` when `--coordinator-release` is given. The authority is the **installed service definition**: it must name this checkout as the release directory, stamp the expected build, and the clean HEAD there must match | `deploy_coordinator_release.rs` — `a_detached_coordinator_release_at_its_installed_sha_is_admitted`, `a_checkout_that_is_not_the_installed_release_is_refused`, `an_installed_build_that_is_not_the_required_one_is_refused`, `a_dirty_release_tree_is_refused`, `a_definition_that_stamps_no_build_is_refused` |

**One row is deliberately empty.** "Roost cannot upgrade the integration asset"
is the only entry on this list with no Rust behind it, and it is recorded that
way rather than quietly dropped: a row that is present and says "not yet, and
here is where" is a promise the repo can keep, and a row that is absent is the
failure mode this table exists to prevent.

### What a release is

A release is a directory whose entries are `bin/` and, when a web bundle is
being shipped, `web/` — and whose `bin/` contains **only** `roost` and
`roost-keeper`. Four separate facts depend on that shape:

- `stage_over_ssh` tars `local_dir.parent()`, so what ships is the release
  root — `deps/`, `build/` and `incremental/` must never be inside it, and
  `web/` rides along with the binaries rather than needing a second transport.
- The target unpacks to `<staging>` and installs `staging/bin` into
  `<release_root>/<sha>/bin`, so `bin/` is where the programs have to be.
- The target recomputes the manifest's `release_digest` over exactly those
  bytes, so the deploying box's digest and the target's must be taken over the
  same two files and nothing else.

**`web/` is optional and its absence is not an error.** A release that
publishes no `roost-web.tar.gz` installs and runs; refusing it would make
deploying an older tag impossible. The probe is the digest **sidecar**, asked
for before the body, so a release that has no bundle costs one small request
rather than a 404 that aborts the deploy. A release that publishes the asset
and has it fail its digest **is** a refusal, and the message names the
expected and actual digests — a truncated download and a tampered one produce
the same "checksum failed" otherwise, and the operator cannot tell which
machine they are standing on.

`web/` lives inside the release directory rather than beside the unit file,
and that is the whole of the retirement story: a settled deploy removes the
prior release directory, so a bundle that outlived its release — and kept
serving a retired UI while `roost status` reported a healthy `spa:` line — is
impossible. `__remote-apply` stamps `ROOST_WEB_DIST_PATH` from the
`release_dir` **it just computed**, never from a path the deploying box
supplied: only the target can say where its own release root is, and a value
decided on the other side is a path into *its* version tree that the next
settlement deletes. It is re-stamped on every install, never preserved.

Cargo does not produce that shape — it writes each binary straight into the
profile directory — so `deploy::release::assemble_release_tree` is the explicit
join between the two, and `RELEASE_BIN_DIR` has exactly one owner,
`deploy::apply_release`. Two constants for one layout is how a deploy ends up
reporting that a release it had just linked is missing.

**Observed, not asserted.** With that layout wrong, `roost deploy` exited **4**
with `the release built for x86_64-unknown-linux-gnu but roost and
roost-keeper missing from .../target/release/bin` — over a build that had
succeeded, in a tree where every test was green. The command had never
succeeded on any invocation.

**`--release TAG`** fetches the target's own published binaries instead of
building them on the deploying box. This is the only way a coordinator can
reach a machine it cannot build for: an x86_64 Linux coordinator cannot
produce an aarch64 or a macOS binary, and three of the production machines are
exactly that. It is a change of *where the bytes come from*, not of what the
deploy does — the same staged tree, the same digest, the same keeper contract
read from the downloaded bytes, the same admission.

The tag **is** this deploy's build identity, and it is proved against the
digest the release published rather than against the deploying box's `HEAD`.
So `--expected-sha`, `--source-root` and `--coordinator-release` are each
**refused** alongside it rather than ignored: every one of them is an
instruction about which build to install, and accepting two answers while
silently preferring one is how a deploy reports a build nobody asked for.

**`--web-dist DIR`** ships a built web bundle in the same staged tree. `DIR`
must hold an `index.html`; a directory without one is refused before anything
is staged, because a worker serving it answers 404 for every URL and reports
itself healthy. Without either flag the behaviour is exactly what it was.

### The keeper admission environment

`roost_keeper::PtyChannel::spawn` no longer inherits the keeper's environment
into every PTY (`command.env_clear()`, `v3` commit `1002ae87`). That fix is
deliberately **not** paired with a `PATH`/`HOME`/`TMPDIR`/`SHELL`/`TERM`
allowlist: the keeper applies exactly what the spec carries, and
`resolve_shell_spec` decides what that is. Admission logic therefore cannot
assume a PTY inherits anything from the keeper, and must not reason as though
`ROOST_KEEPER_CAPABILITY` were present in a shell — it is in the keeper's own
environment, and it is exactly the value that must not leak.

---

## `roost keeper-refresh <host>`

```
roost keeper-refresh <host> [--yes] [--force-live]
```

Shuts a target's keeper down **empty**, under the coordinator's fence: the
whole point of the command is that the keeper is asked to give up its channels
and is not killed holding them. The exit codes are the shared ones in
`crates/roost-cli/src/deploy/codes.rs` — 2 is the only one v2 reserved for this
command, and the rest are shared with `roost deploy` and `roost push` so that a
wrapper can tell "refused, and do not retry" from "failed, try again" without
knowing which of the three it is talking to.
---

## `roost self-link`

```
roost self-link
```

**This entry is a first specification, not a port record.** `roost self-link`
appears in no v2 source and in no earlier revision of this document; the
programme plan named it and it had to be designed. Everything below is
specified here so that the behaviour has an authority that is not the
implementation, and so the Phase 7 cutover can rely on it. Where v2 has
nothing to say, that is recorded rather than papered over.

Makes `~/.local/bin/roost` point at the installed release's `roost`. The
cutover runs it on a machine whose link may be absent, stale, or still pointing
at v2, unattended — so the failure modes below are part of the contract rather
than implementation detail.

**Arguments: none.** The link target is resolved, never configured, so a flag
here would be an argument this command does not have.

**Output.** One line on stdout naming the link, an arrow, and the target —
`<link> -> <target>`, suffixed `(already correct)` or `(replaced a broken link)`
or `(was <previous>)`, so a rerun and a repair are distinguishable without a
second command. On stderr, a `NOTE:` when `~/.local/bin` is not on this
account's `PATH`, because a correct link nothing can find is not a working
install. Exit 0 on all three outcomes.

**The target is resolved, never configured.** It is the `roost` inside the
release directory the **installed service definition** names, and only when
nothing is installed does it fall back to this build's own default program
path. The installed definition is the authority because an operator who moved
the versions directory did it by editing the unit, and the unit is the only
thing that survives.

**What "still pointing at v2" means, and how the command tells.** The target is
compared for **equality against a resolved path**. A v3 release directory and a
v2 one are different directories, so inequality is the fact. It explicitly does
**not** match on a `~/.roost` path prefix: that is a guess about a layout this
command does not own. **The old target is printed by name to stderr before
> repointing**, so a v2 link is visible in the transcript rather than silently
replaced.

**Exit codes.**

| Situation | Code | What it does |
|---|---|---|
| created / repaired / unchanged | 0 | link is or now points at the resolved target |
| the resolved target does not exist | 1 | **no link is created**; names the absolute path it looked for, states the release is not installed, and names `roost quickstart` as the remedy |
| `~/.local/bin/roost` is a regular file | 1 | **refuses**; names the path and says exactly `rm ~/.local/bin/roost` and re-run |
| `~/.local/bin/roost` is a directory | 1 | **refuses**, named. Never `remove_dir_all` on a path under `~/.local/bin` |
| broken symlink | 0 | **repairs** — it carries no content, so replacing it destroys nothing |
| symlink to anything else, v2 included | 0 | **repairs**, after printing the old target to stderr |

**No link is created when the target is missing, deliberately.** A dangling
`roost` on `PATH` is worse than none: it makes `roost` fail confusingly for
every later command rather than fail once, clearly, at the point of
installation.

**A real file is refused rather than clobbered.** It may be the operator's own
script, and they are one `rm` from repairing it.

**The replace is symlink-to-a-temp-name plus `rename`**, so a cutover
interrupted between the two leaves the old link intact rather than a
half-written one. The command is **idempotent**: a second run reports
`(already correct)` and rewrites nothing.

---

## `roost quickstart`

```
roost quickstart [--coordinator-url URL] [--web-dist DIR] [--dry-run]
```

Installs this build's `roost` and `roost-keeper` into the release directory,
installs and waits for `roost3-coord`, installs `roost3-worker`, prints the
status readout, and opens a paired browser. It is the only command that writes
both definitions on a machine that has neither.

**`--coordinator-url URL`** is the HTTPS front door to put in front of the
coordinator. The listener stays on loopback either way; the front door owns TLS
and the forwarded client address, and naming it is what writes
`ROOST_TRUST_PROXY=1` into the coordinator's definition. **On a machine that is
already installed the installed definition wins**, and the flag only promotes
the install to a front door — every other setting is the one the install
already resolved. A rerun therefore cannot silently re-point a machine.

**`--web-dist DIR`** installs a built web bundle into
`<versions>/<ver>/web/`, beside the executables, and writes
`ROOST_WEB_DIST_PATH` into **both** definitions — the coordinator's and the
worker's, because the worker's local door serves the same page. `DIR` must
hold an `index.html`; a directory without one is a **usage error, refused
before anything is written**, because a coordinator serving it answers 404 for
every URL while reporting itself healthy. The directory is the release's own,
so a later release retirement removes the page with the binaries that served
it, and `roost status` — which reports `web_dist_present` separately from
`serves` for exactly this reason — keeps both facts.

Without the flag neither definition names a directory, and that is the honest
state of a machine that was given no bundle: the coordinator writes
`ROOST_WEB_DIST_PATH=` blank (an absent entry would fall back to the service
manager's own environment, which is how a cleared value comes back stale) and
the worker's definition carries no such key at all.

**Log rotation is installed by the first run, not left to the operator.** One
`logrotate.d` entry per role and a shared pair of user units that run it, in
the unit directory systemd reads them from. `copytruncate` because
`StandardOutput=append:` holds the descriptor open — a rename-based rotation
would leave the service appending to an inode with no name. A skip is
**reported, not swallowed**: a machine with no `logrotate` is told its logs
will grow unbounded, because silence there reads as "rotated".

**On macOS this installs nothing, and that is the v2 answer rather than a
gap.** Both v2 installers branch on the platform before this step
(`apps/coord/scripts/install.sh:601`, `apps/worker/scripts/install.sh:518`),
so a macOS account relies on `newsyslog`, which `roost logs` already points
at. The skip line says so by name.

**`--dry-run`** resolves the whole plan, renders both definitions, prints them,
and writes nothing. It runs to completion on a machine with nothing installed,
and `tests/quickstart_dry_run.rs` proves the "writes nothing" half with a
before-and-after tree snapshot rather than a return value.

**stdout** is the answer: the rendered plan and both definitions under
`--dry-run`; under a real run the `roost status` readout, then
`Roost is installed and serving.` with the local origin, the remote origin (or
`optional / unconfigured`), and `roost status` named as the health command.
**stderr** is progress (`>> installing …`, `>> waiting for …`) and the remedy
printed when the paired browser could not be opened. **The one-shot grant is
never printed and never logged** — it is minted, used, and discarded.

**Exit codes.** 0 on a completed install and on a completed dry run. 1 for a
refusal: a front door that is not a usable HTTPS origin, an installed
definition this build cannot parse, a service that did not come up, or a Linux
account whose linger is off and could not be turned on (checked before the
first write; the message is `linger is off for <user>: Roost services stop when
you log out. Run: sudo loginctl enable-linger <user>`). 2 for a
usage error. The deploy codes 5–9 are not raised here: this command calls the
install path directly rather than over ssh, so the keeper-adoption fence does
not apply.

---

## `roost join`

```
roost join
```

Installs and registers **this** machine's worker from a grant. Takes no
arguments: everything it needs comes from the environment, because the command
is pasted into a fresh shell on a machine that has no Roost in it.

| Variable | Meaning |
| --- | --- |
| `ROOST_COORDINATOR_URL` | **Required.** The door the new worker dials. |
| `ROOST_BOOTSTRAP_TOKEN` | **Required.** The one-shot grant `roost add-machine` printed. |
| `ROOST_WORKER_LABEL` | Optional. The name the coordinator shows while this machine is still enrolling. |

The two required variables are reported **one at a time**, in that order, and
the refusal for each names where the other half comes from. A refusal that
listed both would leave the operator guessing which one to go and get.

**The web bundle is downloaded, not assumed.** A joined machine gets
`roost-web.tar.gz` from the same release the running binary came from,
checked against that release's own `.sha256`, installed into
`<versions>/<ver>/web/`, and named in the installed definition. A **source
build** has no published tag, so it installs no bundle and says so rather than
refusing to join: enrollment is the one step a machine cannot do without, and a
missing page is a smaller problem than a machine that is not in the fleet. A
**failed download is a refusal** — silently joining with no page reports
success for a machine that serves 404s.

**Log rotation is installed here too**, by the same code as the first run and
with the same platform rule: nothing on macOS, and a reported skip rather than
silence on a box with no `logrotate`.

**stdout** is four lines naming the installed service, the build SHA this
machine is now identified by, the door it dials, the program path, and
`roost status` as the health command. **stderr** is everything else, including
the bundle and rotation lines above.

**Exit codes.**

| Situation | Code |
| --- | --- |
| installed and registered | 0 |
| either required variable missing, or the door is not a usable origin | 1 |
| Linux account whose linger is off and could not be turned on (same message as `roost quickstart`) | 1 |
| a dirty tree, an unpushed commit, or a checkout that is not the release it claims to be | **7** (`IDENTITY_UNPROVED`) |

7 and not 1 because the remedy is different: a wrapper that retries exit 1
would retry a machine whose checkout cannot be proved, forever.

---

## `roost add-machine`

```
roost add-machine <macos|linux> [--label NAME]
```

Mints a one-shot worker grant against this machine's coordinator database and
prints the one line the new machine runs.

**The door comes from the installed coordinator definition, searched whole
before the environment is searched at all** — `ROOST_COORDINATOR_URL`, then
`ROOST_COORDINATOR_PUBLIC_URL`, then `ROOST_WEB_PUBLIC_URL`. Per-name
interleaving would let a shell that exports the most specific name outrank an
installed definition that declares a different one, which enrolls the next
machine somewhere the operator did not choose and says nothing. The installed
definition wins because it is the only place a front door survives; the
environment answers only for a host with no install. A declared loopback door
is refused rather than printed, because a worker dialing `127.0.0.1` from
another machine reaches that machine's own loopback, which is nothing.

**The database comes from the same two sources in the same order**: the
installed definition's `ROOST_COORDINATOR_DATABASE_URL`, then its
`ROOST_COORDINATOR_DB`, then the same two from the environment — which is how
`add-machine` runs inside a coordinator container, whose environment is its
definition. A SQLite file must already exist; a Postgres URL is the one the
coordinator itself boots with.

**The printed command is a credential being pasted into a shell**, so every
value it carries is one single-quoted word, whatever the value contains.
`tests/add_machine_enrollment.rs` proves that by reading the printed line the
way a POSIX shell reads it and unquoting it back to the hostile URL and label
it was given.

**stdout** is `Run this on the new <platform>:`, a blank line, the command, and
a blank line — kept copy-pasteable, which is why the key-loading chatter goes
to stderr instead. **stderr** carries that chatter and the line naming the
coordinator the grant was enrolled against.

**Exit codes.** 0 on a minted grant. 1 for a refusal: no declared door, a
declared loopback or non-HTTPS door, no installed coordinator database to
mint the grant against, or a `--label` carrying a control character. 2 for a
usage error, including `windows` as the platform — v3 ships no Windows host
install to enroll, and the refusal says so.

---

## `roost add-browser`

```
roost add-browser [--label NAME]
```

Mints a one-shot browser grant against this coordinator's database and prints
the pairing URL that spends it: `<origin>/#pair=<grant>`, the same shape
`roost quickstart` opens. It is how the first browser pairs with a coordinator
no desktop can open — a container, a VM, a server reached over SSH.

The database resolves exactly as `add-machine`'s does. The origin is the
declared `ROOST_WEB_PUBLIC_URL` (installed definition, then environment), else
`http://127.0.0.1:<port of ROOST_COORDINATOR_BIND>`. The grant rides in the URL
fragment, which a browser never sends to a server.

**stdout** is the URL and nothing else; **stderr** notes that the grant is
one-shot and accepted for 24 hours. **Exit codes.** 0 on a minted grant; 1 for
no resolvable database, an unusable declared front door, or a `--label`
carrying a control character.

---

## `roost push`

```
roost push
```

Proves this checkout's commit, publishes it, holds the local coordinator onto
it, and converges the whole fleet in **one journaled transaction with a single
decision boundary**. It takes no arguments on purpose: a push with a flag is a
push that was asked for something other than the whole fleet, and the fleet is
the unit the transaction commits or does not.

**The order is the safety property.** Nothing is mutated until the commit is
proved and published, the registry's identity is whole, every participant is
classified, and at least one of them is safe to touch. Then the local
coordinator is held, and only then does the rollout converge. Every refusal
above the hold leaves the fleet exactly where it was.

**A failure before the durable decision rolls the whole fleet back**,
exhaustively — the machine that failed and every machine already moved, then
the coordinator, then a fresh proof of the prior commit. Past the decision
there is no rollback, and a failure there is the one situation exit 8 exists
for. A machine that cannot be converged *now* is **deferred, not failed**:
not reachable, stale, or on a different commit, each with its own reason, and
reported beside the success rather than counted as converged.

**stdout** is the one line the operator asked for plus the deferred report.
**stderr** is progress (`>> git push`, `>> stage the coordinator release …`,
`>> converging N participants`). **Everything the fleet did is a `tracing`
event**, never a line on either stream.

**Exit codes.** The shared install/deploy codes, so one wrapper can read a
refusal from `deploy`, `keeper-refresh` and `push` without knowing which it
is talking to: **1** generic, **2** rejected invocation or SSH unreachable,
**5** a keeper could not be adopted, **6** no coordinator URL and no prior
install to reuse one from, **7** the build identity could not be proved,
**8** the transaction reached its irreversible point and could not be settled
(including a decision that could not be recorded at all), **9** a remote
process or lease died mid-transaction.

---

## `roost api <verb>`

```
roost api <verb> [<arg>...] [options]
```

Introspects and drives a running coordinator without a browser. Every method is
called through the **generated** Connect client; there is no hand-rolled method
name anywhere in the verb table, because a hand-written path is a second answer
to "what does the coordinator call this RPC" beside the one the coordinator
serves.

**Each verb parses its own arguments**, because the shapes disagree: `input`
takes text that must not be read as an option, `ws-set-sessions` takes a list
that runs until the next option, and `rename` takes a title an operator types
as several words. One grammar cannot be given to all of them, and a grammar
invented here would be a second answer beside the table in `api::verbs`.

| Verb | Takes |
| --- | --- |
| `agents`, `sessions`, `workers`, `cells`, `tasks`, `workspaces` | filters only |
| `agent-status <session>` | `--json` |
| `agent-wait <session>` | `--until STATES --timeout DURATION` |
| `agent-prompt` | the prompt, plus its proof flags |
| `input` | `<text>` or `--stdin`, plus `--enter` |
| `spawn`, `attach`, `assign`, `kill` | the session arguments each needs |
| `rename` | `<id> <title…>` |
| `ws-create`, `ws-update`, `ws-delete`, `ws-set-sessions` | the workspace id and its own flags |
| `ui` | a sub-command, plus `--tab`, `--first`, `--off` |
| `ui-state`, `tasks`, `task-enqueue`, `task-cancel`, `device-revoke-local` | see `api::verbs` |

**`events` is removed**, and `cat` and `watch` are **tombstones**: they remain
answerable, and answer with a refusal naming the replacement — `cells` for
scrollback, and nothing for the live output stream — at **exit 1**. A stale
script that still types them gets told what to use; a usage error about a name
that used to work would only tell it the name is wrong.
**stdout** is whatever the verb produces, and for the one verb that produces
JSON (`agent-status --json`) it is the only thing on the stream. **stderr**
carries diagnostics. The task queue's payload column collapses every run of
whitespace to one space **without trimming**, so a pretty-printed document
still occupies one row and a stored payload compares as it was written.

**Exit codes.** 0 when the coordinator answered. 1 for a transport or
coordinator failure. 2 for a usage error: no verb, an unknown verb, or a
malformed argument — each prints the verb table.

---

## `roost dev`

```
roost dev
```

Runs the coordinator, the worker and the web dev server as three children of
this process, for a checkout. Takes no arguments: everything it needs is
resolved, and an argument here would be one whose value this command would have
to keep in step with `roost coord` and `roost worker`.

**SIGINT and SIGTERM fan out to all three.** The termination watch is installed
**before the first child exists**, so a signal arriving during startup is caught
instead of ending this process with children attached. A child that exits on
its own stops the rest, because a dev stack with one server missing is a
mistake someone would otherwise read as a hang.

**Nothing is exported into this shell.** The dev coordinator's boot is resolved
through the same resolver `roost coord` uses, against an environment overlaid
in memory, because a variable that reached the loader by being exported would
outlive the command and describe this machine's dev identity to whatever ran
next in the same shell.

**stdout and stderr are the children's.** This command prints no readout of its
own: the three dev servers' output *is* the output, and a line of its own
interleaved into a web dev server's build log helps nobody. Its own state
transitions are `tracing` events. **Exit code 0** on a clean stop, whatever
signal asked for it; 1 when the stack could not be started at all.

## `roost import-v2`

```
roost import-v2 --from PATH [--dry-run]
```

Carries a v2 coordinator's account, paired devices and browser keys into this
install, **once**, and is the only code in the tree that opens a v2 database.
The coordinator never does: that is the invariant which keeps a v3 install from
growing a migration path it would have to support forever.

**It exists because browser keys are origin-bound.** A paired browser cannot
be handed to another origin and expected to work, so the only way a browser
survives the cutover is for its key to already be in the v3 database. This
command puts it there, and the Stage 4 proof that a paired browser keeps
working is the proof that it did.

**It must run before anything creates the v3 database.**
`ensure_self_hosted_tenant` creates a fresh account in an empty database, and a
later import could not reconcile that — the account it carried across would be
a second one, and the coordinator refuses two. The refusal below enforces the
half an operator can get wrong.

**What is copied**, from the v2 schema, and the column lists are column for
column identical: `accounts`, `account_identities`, `organizations`,
`organization_memberships`, `dashboards`, `dashboard_memberships`,
`account_devices`, `app_settings`, and `authorized_key_revocations` in full.
`authorized_keys` is copied **only where the fingerprint is one an account
paired** — 26 of the 31 rows on the database this was written for. The other
five are machine keys whose fingerprint is a `workers.fp`; importing them would
enrol five authenticators that no human paired and no browser can present.

**What is not copied, and the reason each is not:** `sessions` (terminal
sessions bound to v2 workers, not authentication), `events` and `audit_log`
(~1.5 M rows of the product being replaced), `workers`, `bootstrap_tokens`,
`tasks`, `pair_requests`, `email_outbox`, `mcp_relays`, `workspaces`,
`workspace_sessions`, `push_subscriptions`, the token and redemption tables,
`feature_flags`, and `_migrations` — v3's own migrations have already run by
the time any of this executes.

**Mechanics, each of which is load-bearing.**

- The source is ATTACHed **read-only** (`mode=ro` in the filename, not left to
  file permissions) and read inside the same transaction that writes, so the
  import sees one consistent snapshot of a file the v2 coordinator is still
  writing to. A device paired during the read is either wholly in the snapshot
  or wholly out of it.
- Each table is copied by a single `INSERT … SELECT`, so SQLite performs the
  copy rather than a reader and a writer marshalling values between two
  representations of the same row.
- The target is opened with **roost-coord's own** `db::open`, so v3's
  migrations run and the file is the one the coordinator will read.
- The column list is read from the **target's** `PRAGMA table_info`, so a
  column v3 has and v2 lacks fails loudly inside a transaction that rolls back,
  rather than a row that quietly loses a field.
- After the commit, the coordinator's own `ensure_self_hosted_tenant` runs
  against the target and must return the account the import carried. That call
  is the validator, and it is why the order above is not negotiable.

**A re-run is a refresh, not a merge.** A target holding exactly one account
that is not the one being imported is **refused by name** — two accounts in
one coordinator is a state v3 does not have, and the repair is for the
operator to say which install the machine is. Otherwise new
`account_devices`/`authorized_keys` rows are added, new revocations are
applied, the rows a revocation covers are deleted, and `app_settings` is left
alone where v3 already has a row — so an operator who has already changed the
Deepgram key or the VAPID pair in v3 does not have it reverted by re-running an
import. This is how the production flip picks up devices paired on v2 during
the cutover window.

**stdout** is one line per table, `<table>: <n> copied, <m> already present`,
under a first line saying whether this was a first import or a refresh. Those
are the numbers a real run produced, not an estimate: a run inserts exactly the
rows the target did not have. **stderr** is nothing; every failure is the one
JSON failure line described under [The failure line](#the-failure-line).

**Exit codes.**

| Situation | Code |
| --- | --- |
| imported, or dry-run reported | 0 |
| `--from` is not a file; the coordinator is running; the target belongs to another install; the target is not a single-account install | 2 |
| the source has no account, or more than one; the source could not be attached; a statement failed; the imported topology is not a valid self-hosted install | 1 |

---

## `roost db-to-postgres`

```
roost db-to-postgres [--from PATH] [--to URL] [--replace]
```

Copies a v3 coordinator's SQLite database into a Postgres database, so the
coordinator can run stateless against `ROOST_COORDINATOR_DATABASE_URL`. The
copy is `roost_coord::db::sqlite_to_postgres`, in the crate that owns both
schemas.

**Defaults.** `--from` is the file this host's installed coordinator declares
in `ROOST_COORDINATOR_DB`, then the one this shell exports, then the default
data directory's. `--to` is `ROOST_COORDINATOR_DATABASE_URL` from this shell,
which keeps the password out of the process list; it must be a `postgres://`
or `postgresql://` URL.

**Both ends are migrated first, by the coordinator's own `db::open`**, so the
source and the target carry this build's schema. The table set, each column's
Postgres type and the foreign-key order are read from the target's catalog and
checked against the source's: a table or column on one side only is a
refusal, not a dropped field.

**One transaction.** User triggers are disabled for the copy (they re-check
invariants the SQLite file's identical triggers enforced when each row was
written, and would make insert order part of correctness); foreign keys stay
on. Rows move in batches of 2 000 through one `INSERT … SELECT FROM UNNEST`
per batch; every identity sequence is moved past the largest copied id; every
table's copied count is checked against the source's. A failure anywhere
rolls the target back untouched.

**A target holding rows is refused** — a coordinator already booted against
it creates its own account, and a second one is a state v3 does not have.
`--replace` empties every coordinator table first, inside the same
transaction. Stop the coordinator using the target before either.

**The source must be idle.** When `--from` is this host's installed
coordinator's file and that service is running, the command refuses: rows
written mid-copy would be missing from the target with nothing to say so.

**stdout** is the source path, then one line per table with its row count,
then the total and the elapsed time. The URL is never printed.

**Exit codes.**

| Situation | Code |
| --- | --- |
| copied | 0 |
| no target URL, or not a Postgres URL; the source file is missing; the target holds rows and `--replace` was not given; the installed coordinator is running on the source | 2 |
| either end could not be opened or migrated; the schemas disagree; a value does not fit its column; a count mismatch; a statement failed | 1 |

---

## `roost update`

```
roost update
```

Replaces **this binary** with the latest published v3 release, atomically, on
this machine. It takes no arguments: what to install, from where, and whether
that is safe here are all decisions, not configuration.

**The order is the safety property.** An interrupted update is resolved
**first**, because a machine that lost power mid-swap already has a binary in a
state this run must account for before it makes a second change to it. Then the
release is resolved, the candidate is downloaded and proved against the
release's published digest, the running keeper is admitted, and only then does
the rename happen.

**Only a `v3.` tag is installable, and pre-releases count.** This repository
publishes both series from one tag namespace, so "the newest release" is today
a TypeScript `roost` — and a digest-verified download of it passes every check
this command makes while replacing a Rust binary with a Bun one that answers
none of the commands in this document. Resolution is the GitHub releases
**listing** filtered to `v3.`, drafts excluded, and the download names the tag
it chose: a `latest/download` URL would fetch whatever series published last.
Until `v3.0.0` exists the fleet runs `v3.0.0-rc.N`, so a pre-release is the
answer and a final release is not a filter.

**The web bundle is swapped with the binaries.** After the rename, the
release's `roost-web.tar.gz` is downloaded and checked against the same
release's digest, and unpacked into the release directory the replaced binary
lives in. Both installed definitions already point at that directory, so
neither is rewritten. A swap that moved only the binary would leave a machine
running the new coordinator over the old page — an index referencing asset
hashes the new build does not ship, which loads its shell and then fails every
request for its code.

A binary **outside a release tree** — a tarball dropped in `~/bin`, which is
how a machine with no version directory at all was installed — has no bundle
directory to put one in. That is reported by name rather than worked around,
because the alternative is writing `~/web` and leaving the operator to find it.
A failed bundle download is likewise reported and does not fail the update: the
swap already settled, the page is a second problem, and a refusal here would
report a completed update as failed and send the operator to re-run a swap that
has already happened.

**`ROOST_RELEASE_BASE_URL`** replaces the download origin for a mirror, exactly
as the deploy paths use it. A listing with no `v3.` tag in it is
`no published release to update to` on stdout and exit 0 — the same sentence a
v2-only repository produces, and not a v2 download.

**This command replaces `roost` and nothing else.** It does not write
`roost-keeper`, signal a keeper, or restart a worker, so a running keeper
holding live PTYs is not disturbed by the swap. What the keeper gate decides is
whether the CANDIDATE's keeper contract is one the running keeper cannot live
under — and the candidate is interrogated **by running it**, never by asking
this process, because a contract read from the binary that happens to be
installed is a contract for the wrong program.

**stdout** is the answer: `>> updated to <version>`, the line saying the
running services keep the old binary until they restart, and one line about
the keeper — preserved with its worker fingerprint, or `no keeper was running
here` with the reason. `>> already the latest release (<tag>)` when there is
nothing to do. **stderr** carries nothing: every failure is the one JSON
failure line described under [The failure line](#the-failure-line).

**Exit codes.** 0 for updated, already latest, or no published release. 1 for
every refusal, and each names its remedy: a source build refuses to replace
itself; an interrupted update that was rolled back is **not** retried
automatically, because re-running the command is a decision; Windows is
refused by name. **2** for a usage error.

---

## The subcommand count is 26, and three numbers are each correct

`crates/roost-cli/src/lib.rs`'s `Command` enum has **26 variants**, and both
`name()` and `dispatch()` answer all 26 — checked arm by arm, so there is no
orphan variant and no arm without one. 21 are visible; 5 are hidden
(`__keeper-contract` and the four `__remote-*`).

- **21** is v2's operator-and-daemon surface: v2's `main.ts` carried 23
  subcommand keys, minus 2 deliberately dropped (`cutover`,
  `__windows-updater-broker`). v3 adds 4 v2 did not have — the `__remote-*`.
- **26** is what the v3 dispatcher actually answers, and it is what
  `tests/command_tree_shape.rs` asserts, because that file's own doc says its
  list is *"every subcommand the crate's dispatcher answers"* and it exists
  precisely because a deploy's journal addresses `__keeper-contract` by string
  and a deploy addresses the `__remote-*` over ssh. **Asserting 21 would drop
  exactly the five commands the file exists to protect** — and it dropped one:
  `roost update` had a complete implementation and no `Command` variant, so the
  shape test reported a full surface while nothing could invoke the command.
- **26** is also the number of subcommands *this document* names under its own
  `` ## `roost …` `` headings, in 24 sections — the three server modes share
  one because they share an output contract, and a heading may carry its
  argument (`roost deploy <host>`). The same test asserts the mapping is total
  in both directions against `SUBCOMMANDS`: every command named by exactly one
  section, and every section naming at least one command. A command in the tree
  with no section, or a section for a command this build does not answer, is a
  contract that has drifted from the product.

Three correct numbers about three different objects, which is why the count
disagreed across briefs, the test, and this contract. **26 is the answer.**

---

## The target side: `roost __remote-*`

Four hidden subcommands, all `#[command(hide = true)]`, all reading and
writing **standard input and stdout**. They are what `roost deploy <host>` runs
*on the target*, over ssh, and they are addressed by string — a deploy's
manifest names them, so a rename breaks a deploy rather than a person. They are
each specified below because a machine that has never run a deploy cannot tell
from this document that this is what a deploy invokes on it, and an operator
who types one by hand is running a machine mutation outside the transaction
that exists to make it safe.

## Deliberately dropped from v2

Two of v2's 23 command keys are **not** in v3, and their absence is a decision
rather than an omission. A missing subcommand is a usage error, not a stub.

| Command | v2 arguments | v2 exit codes | Why v3 does not have it |
| --- | --- | --- | --- |
| `roost cutover` | `--force` | 2, 3 | It migrated `coordinator.db` → `coordinator_v2.db`. v3 is a fresh install with a new data directory (`RoostCoordinatorV3`) and its own database name, so there is no v1 database to migrate and no code path that could open one. |
| `roost __windows-updater-broker` | none | — | v3 ships Linux and macOS only. The broker existed to service a Windows self-update request from a privileged helper; with no Windows host install there is nothing to broker. |

---

## `roost __remote-facts`

```
roost __remote-facts
```

Reports what this machine has installed, for the deploy that asked.

**Arguments: none.** The facts are read from the **process environment of the
command that spawned this one**, not from a scan of the machine: a probe that
looked around on its own would report whatever it found first, and the deploying
box's composed environment is the only statement of intent there is.

**stdout** is the encoded facts document behind its own prefix, and nothing
else. **stderr** is the one JSON failure line. **Exit codes.** 0 when the
facts were read and encoded; 1 when the platform is not one this product
installs on, or the document could not be encoded.

## `roost __remote-evidence`

```
roost __remote-evidence
```

Answers one question: may a release be staged on this machine, and what would
staging it destroy?

**Arguments: none.** It runs one command — the service manager's own status for
this machine's worker label — and reports the markers that answer whether a
staged release has anything to destroy.

**stdout is the command's own output, plus the markers**, and that is
deliberate: a deploy parses the markers out of what the service manager
reported, and capturing the run would mean inventing a second rendering of it.
**Exit codes.** 0 when the evidence was gathered; 1 when the service label or
the definition path could not be resolved for this platform.

## `roost __remote-transaction`

```
roost __remote-transaction --kind KIND
```

Takes **the machine transaction** — the lock that makes a deploy the only
thing mutating this machine — says so, and then **holds it until stdin closes**.

`--kind KIND` records what the holder is doing, so the next operator reading
the lock file knows whether a deploy or a recovery held it.

**The wait is the mechanism, not an artefact of it.** Closing this process's
input is how the holder says it is done, and **the kernel releasing the file
lock** is how the machine notices when the holder dies without saying anything.
So a deploy that loses its ssh connection does not leave a machine locked — the
lock dies with the process. That is the whole reason the command is a process
that waits rather than a flag with a timeout, and it is not visible from the
name.

It blocks on a **blocking thread** rather than through tokio's stdin, which is
behind a feature this crate does not enable: a transaction holder is a process
whose only job is to wait, and the thread it waits on is idle by construction.

**stdout** says the transaction was taken, naming its kind. **Exit codes.** 0
when it was taken and then released; 1 when the lock is already held, naming
the holder; 2 for an unknown `--kind`.

## `roost __remote-apply`

```
roost __remote-apply
```

Installs the release the **manifest on standard input** names, and prints the
report the deploying box reads.

**Arguments: none** — everything it acts on arrives on stdin, because a
mutation this deep must be described by the transaction that took the lock, not
by argv a second deploy could have written differently.

**stdout** is the encoded report behind its own prefix. **Exit code 0 for both
a refused apply and a rolled-back one**, because the report IS the answer and
the exit code is the operator's; a caller that treated a non-zero exit as "no
report" would have to guess at what went wrong. A failure to read the manifest
or encode the report is 1.

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
