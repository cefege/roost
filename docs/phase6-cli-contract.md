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

## `roost deploy <host>`

```
roost deploy <host> [--label LABEL] [--reachable-addr ADDR]
                  [--source-root DIR] [--expected-sha SHA]
                  [--expected-manifest-sha256 HEX]
                  [--allow-unpublished-local] [--coordinator-release]
                  [--force-live]
```

The one command in this crate that replaces the binary every live PTY on a
machine depends on. Its order is the safety property and is stated once, in
`crates/roost-cli/src/deploy/run.rs`: prove what is being shipped, probe the
target, **ask the coordinator whether the target's keeper may be carried across
BEFORE the target's definition is replaced**, and only then touch the machine.
The keeper question is asked early and acted on early because its answer decides
whether the machine may be touched at all; the definition is replaced last
because that is the only step that is hard to put back on its own.

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
| A one-shot deploy flag stops at the installer process | `deploy/identity_env.rs::worker_install_environment` strips `ROOST_BOOTSTRAP_TOKEN` **and** `KEEPER_FORCE_LIVE_RETIRE_ENV` from the prior install; `deploy/invocation.rs::definition_environment` inserts the retire grant only when `--force-live` was actually given, so a value reaches the service only when the definition carries it | `deploy_remote_identity.rs` — `a_deploy_never_carries_a_one_shot_grant_forward`; `services_definition_text.rs` — `a_one_shot_grant_is_never_carried_into_a_definition` |
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

A release is a directory whose **only** entry is `bin/`, and whose `bin/`
contains **only** `roost` and `roost-keeper`. Three separate facts depend on
that shape:

- `stage_over_ssh` tars `local_dir.parent()`, so what ships is the release
  root — `deps/`, `build/` and `incremental/` must never be inside it.
- The target unpacks to `<staging>` and installs `staging/bin` into
  `<release_root>/<sha>/bin`, so `bin/` is where the programs have to be.
- The target recomputes the manifest's `release_digest` over exactly those
  bytes, so the deploying box's digest and the target's must be taken over the
  same two files and nothing else.

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

## Not in the tree yet

These are part of the Phase 6 command list and are **not implemented in this
slice**. They are recorded here so whoever implements them has the v2 contract
to port, and so nobody discovers their absence at runtime. Nothing in the tree
pretends to be one of them: a missing subcommand is a usage error, not a stub.

`roost deploy <host>` and `roost keeper-refresh <host>` **were** on this list and
have moved above, with their argument table, their guard map and their observed
exit codes. They are the two commands that change another machine, so they are
documented next to the deploy guards rather than in a list of what is missing.

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
