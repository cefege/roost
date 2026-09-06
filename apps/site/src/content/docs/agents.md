---
title: "Agents and status"
description: "Roost never owns the agent process. How ten CLIs get first-class status detection, the three detection tiers, and why nothing about status is persisted."
order: 5
section: "Concepts"
---

## Roost never owns the agent

Every Roost session is a shell PTY, and an agent CLI is an ordinary command
running inside it. Roost does not spawn, supervise, or own an agent process,
conversation, transcript, tool call, or approval model. There is no wrapper
process or composer. Authenticated agent-specific RPCs only read worker-observed
status; they do not control the agent.

That is a deliberate boundary, and it is what makes "any CLI" true rather than
aspirational. Anything that runs in a terminal runs in Roost: a shell, a REPL,
`vim`, `htop`, a build, `ssh`, tmux if you want it — and any coding agent,
including Claude Code. What the list below adds is a *status badge*, not the
ability to run.

## The launcher list

Ten agents are first-class in the launcher and in status detection. Each is
launched by its own CLI binary:

| Agent | Command |
|---|---|
| OpenAI Codex | `codex` |
| Gemini CLI | `gemini` |
| OpenCode | `opencode` |
| Cursor Agent | `cursor-agent` |
| Amp | `amp` |
| GitHub Copilot CLI | `copilot` |
| Droid | `droid` |
| Grok CLI | `grok` |
| Pi | `pi` |
| OMP | `omp` |

Beside them is a custom-command entry: type any command and Roost launches it in
the session the same way. A custom command runs perfectly well; it simply is not
labelled with an agent identity.

## What a status means

Roost labels a shell PTY with the state of whatever coding agent happens to be
running inside it: `working`, `blocked` (needs input), or `idle`. That is metadata
*about a terminal*, not a structured agent session.

In the UI those three states read as **working**, **needs input**, and **done**,
and they appear in four places: the session's sidebar row, its tab, its card on
mobile, and a rollup on the folder that contains it — for example
`2 working · 1 needs input`. Plain shells stay unmarked; an unlabelled terminal is
the normal case, not a failure.

## Read status from the CLI

An authorized CLI identity can read the current status rows in its selected
dashboard without opening a browser:

```sh
roost api agent-status <session> [--json]
roost api agents [--json]
roost api agent-wait <session> --until <blocked,idle,working> --timeout <duration>
```

The first command reads one authorized session; a missing or foreign session
has the same not-found result. The second lists current rows in `session_id`
order. Human output is headered TSV. In that view an absent legacy source is
shown as `legacy`; `--json` instead returns an explicit machine projection with
exactly `session_id`, `agent_id`, `state`, `message`, `status_epoch`,
`occupant_id`, `source`, `revision`, `completed_revision`, `updated_at`, and
`promptable`. Missing message and identity fields are JSON `null`, while
revision and timestamp fields are numbers.

`status_epoch` identifies one worker status-registry lifetime, `occupant_id`
identifies one worker-verified process incarnation, and `source` records whether
the state came from an integration or screen observation. They are volatile
observation and fencing state, not process handles, agent credentials, or
conversation identifiers. Process IDs never leave the worker. Only a complete
integration identity is `promptable`; screen and legacy rows remain readable
with `promptable=false`.

`agent-wait` first reads and pins the current `status_epoch` and `occupant_id`,
then registers an event-driven wait at the coordinator. `--until` is a unique
comma-list of `blocked`, `idle`, and `working`; `--timeout` accepts an integral
`ms`, `s`, or `m` duration through five minutes. It prints exactly one terminal
outcome: `matched`, `timed_out`, `occupant_changed`, or `session_closed`.
Only `matched` exits successfully. Replacement, close, and fast transitions are
observed in the status hub; the command does not poll or scrape terminal output.

## Three detection tiers

Detection lives entirely on the worker, next to the PTY it is describing.

**1. Process scan.** A periodic `ps` pass identifies a known agent binary in the
session's process tree. This is what makes an agent you started by hand — not
through the launcher — still get recognised.

**2. Integration reports.** OMP and Pi report their own lifecycle, including
"waiting on you" and retry grace, over a per-worker local endpoint. Every
spawned PTY receives the endpoint and `ROOST_SESSION_ID` in its environment.
The server kernel-attests the accepted socket's peer process ID, then a fresh
process-tree scan must prove that exact process is the current known agent
under the capability's session. Process identity and ordering never come from
report fields.

**3. Screen and title manifests.** Terminals with no integration — the other
agents, and sessions that predate an install — fall back to scanning their own
screen contents and OSC title/progress output against pinned per-agent manifests.

**Attribution.** Roost's screen and OSC-title detection manifests, its
process-backed detection, and its first-party OMP and Pi lifecycle integrations
were adapted from herdr (herdr.dev) at commit `eacea2da` under
Apache-2.0. The attribution sits in the source headers of
`apps/worker/src/agent-status/`. herdr solved agent-state detection well, and
Roost's detection is downstream of that work rather than an independent
invention.

## Precedence and arbitration

An integration report beats the screen: if OMP says it is blocked, that is the
answer, regardless of what its output looks like. A silent integration's lease
expires after 30 seconds, at which point the session falls back to screen
detection automatically — so a crashed reporter degrades instead of freezing a
badge.

The worker publishes exactly one *effective* state per session. A status epoch
identifies the worker registry lifetime, an occupant identifies the observed
process incarnation, and revisions are monotonic within that identity. The
coordinator scopes staleness checks to all three values, so a replacement worker
or process can begin at a lower revision without an older frame taking over.

## Nothing about status is persisted

Status frames travel worker to coordinator, into an in-memory hub ordered by
status epoch, occupant, and revision, out over the sync stream, and into the
browser store. A fresh sync connection is seeded from the hub snapshot, and
closing a session drops its record.

Because there is no persistence, a worker, coordinator, or browser restart
*converges* rather than leaving a stale badge behind. There is no cache to
invalidate and no cleanup job. The one persisted piece in this whole subsystem is
the set of push subscriptions, because a device has to stay subscribed across
restarts to be notified at all.

## Notifications

The coordinator classifies only background transitions — `working → blocked` and
`working|blocked → idle` — and after a one-second cancellable delay sends a Web
Push to subscribed devices that are **not** currently viewing that session.
Opening the session cancels a pending notification and acknowledges its revision,
so walking over to a terminal does not also buzz your phone.

Everything else is browser-local and needs no permission grant: the in-app toast,
the unseen count in the tab title, an optional sound, and a per-browser-profile
claim so two tabs of the same profile deliver one notification instead of two.

OS-level notifications need one explicit grant per device, because browsers only
prompt on a real click: **Settings → Notifications → Desktop notifications**. On
iPhone and iPad, install Roost to the Home Screen first and open it from there —
Safari only allows notifications for installed web apps. See
[mobile](/docs/mobile/) for the rest of the phone story.

## What this is not

Roost is not an orchestrator. It does not restart a stuck agent, compact its
context, track its token spend, or schedule it. It tells you which terminal wants
your attention and gets you into that terminal from any device. If you want
unattended supervision, a cost ledger, or a task board, that is a different tool
— [alternatives](/alternatives/) says which ones and when to prefer them.

## Next

- [Mobile](/docs/mobile/) — notifications, the key row, and the card deck
- [The terminal](/docs/terminal/) — the PTY the agent actually runs in
- [Fleet](/docs/fleet/) — where detection runs and why
- [Alternatives](/alternatives/) — orchestrators, and when to use one
