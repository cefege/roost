---
title: "Agents and status"
description: "Roost never owns the agent process. How ten CLIs get first-class status detection, the three detection tiers, and why nothing about status is persisted."
order: 5
section: "Concepts"
---

## Roost never owns the agent

Every Roost session is a shell PTY, and an agent CLI is an ordinary command
running inside it. Roost does not spawn, supervise, or own an agent session. There
is no wrapper process, no transcript store, no composer, and no agent-specific
RPC.

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

## Three detection tiers

Detection lives entirely on the worker, next to the PTY it is describing.

**1. Process scan.** A periodic `ps` pass identifies a known agent binary in the
session's process tree. This is what makes an agent you started by hand — not
through the launcher — still get recognised.

**2. Integration reports.** OMP and Pi report their own lifecycle, including
"waiting on you" and retry grace, over a per-worker Unix socket. Every spawned PTY
receives `ROOST_AGENT_SOCKET_PATH` and `ROOST_SESSION_ID` in its environment, and
the report server validates that the reporting process id genuinely belongs to
that session before accepting a report. An agent cannot claim to be a session it
does not own.

**3. Screen and title manifests.** Terminals with no integration — the other
agents, and sessions that predate an install — fall back to scanning their own
screen contents and OSC title/progress output against pinned per-agent manifests.

**Attribution.** Roost's screen and OSC-title detection manifests, its
process-backed detection, and its first-party OMP and Pi lifecycle integrations
were adapted from [herdr](https://herdr.dev) at commit `eacea2da` under
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

The worker publishes exactly one *effective* state per session, carrying a
monotonic revision so out-of-order frames cannot regress it.

## Nothing about status is persisted

Status frames travel worker to coordinator, into an in-memory revision-ordered
hub, out over the sync stream, and into the browser store. A fresh sync
connection is seeded from the hub snapshot, and closing a session drops its
record.

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
