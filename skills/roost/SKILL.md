---
name: roost
description: "Inspect and coordinate observed coding agents in ROOST through the authenticated CLI. Use when the user explicitly asks to inspect or control ROOST sessions, or to coordinate another agent already running in ROOST. Requires ROOST_SESSION_ID."
---

# ROOST

ROOST keeps ordinary shell PTYs alive across worker restarts and exposes dashboard-authorized CLI reads, waits, and status-fenced prompt input. It does not own an agent process, conversation, transcript, tool call, or approval flow.

## Establish caller context

Act only from a ROOST-managed PTY. Verify the injected session ID first:

```sh
test -n "${ROOST_SESSION_ID:-}"
```

If that check fails, say that this process is not running in a ROOST session and stop. Do not substitute the session focused in a browser or guess an ID from a title.

The CLI identity is dashboard-scoped, not PTY-scoped. It can therefore see resources beyond the caller's session. `$ROOST_SESSION_ID` identifies only the current shell PTY; it does not narrow the CLI key's authority.

Before any mutation, require a fresh authenticated command to exit successfully and return parseable JSON:

```sh
roost api agents --json
```

For an operation involving the current session, also require a successful point read whose returned `session_id` exactly equals `$ROOST_SESSION_ID`:

```sh
roost api agent-status "$ROOST_SESSION_ID" --json
```

A failed command, non-JSON output, missing row, or mismatched ID is a hard stop. Do not mutate first and inspect afterward. Do not inspect CLI or worker private keys, report-capability files, local report sockets, conversation references, or terminal transcripts to bypass this gate.

## Keep ROOST identities separate

Use the exact identifier returned by an authenticated API response:

- A **machine** is the host computer.
- A **worker** is the ROOST process on one machine, identified by its fingerprint. A worker is not a terminal.
- A **workspace** is a named folder-backed grouping of sessions.
- A **session** is the user-facing ordinary shell PTY. `$ROOST_SESSION_ID` is a session ID.
- A **channel** is the keeper's internal PTY binding for a session. Do not use a channel ID where an API asks for a session ID.
- A **pane** is a browser-local layout region that displays sessions. It does not own the PTY.
- A **browser tab** is one browser client with its own reported UI state and layout. It is not a session, channel, or pane.

Never target by sidebar order, display title, agent kind, focused pane, or an ID copied from an example. Preserve a returned ID as one shell argument and pass it explicitly.

In the examples below, replace the quoted `<target-session-id>` placeholder with one exact `session_id` from the fresh JSON response. If no exact ID was selected, stop rather than falling back to any focused resource.

## Inspect observed agent state

List dashboard-visible observed agents as stable JSON:

```sh
roost api agents --json
```

Inspect one exact session before controlling it:
```sh
roost api agent-status "<target-session-id>" --json
```

Confirm that the response names the requested `session_id`. `status_epoch`, `occupant_id`, and `revision` fence one volatile worker-observed process incarnation. They are not conversation identifiers. Only a row with `source` equal to `integration` and `promptable` equal to `true` can accept a guarded prompt. A screen-only or legacy row is readable but not promptable.

## Wait without polling

Wait on the exact occupant currently observed in a session:
```sh
roost api agent-wait "<target-session-id>" --until idle,blocked --timeout 5m
```

The terminal outcome is `matched`, `timed_out`, `occupant_changed`, or `session_closed`. Only `matched` exits successfully. A match proves that ROOST observed one requested state; it does not prove that a task, tool call, or approval completed. Do not poll terminal output or scrape a transcript instead.

## Send one status-fenced prompt

After the fresh JSON inspection, send one prompt to the exact session:
```sh
roost api agent-prompt "<target-session-id>" "Review the current diff and report only actionable findings."
```

To register an occupant-pinned wait with the same prompt:
```sh
roost api agent-prompt "<target-session-id>" "Review the current diff and report only actionable findings." --wait --until idle,blocked --timeout 5m
```

The input outcome and optional wait outcome are separate. `accepted` proves that the fenced PTY input was accepted, not that the agent executed it successfully. `rejected` proves no prompt write occurred. `ambiguous` means the write may have occurred: never retry it automatically. A blocked, replaced, expired, screen-only, closed, or newer-revision occupant is refused at the guarded boundary.

Do not use raw terminal input to answer an approval or question. Ask the user. Use raw input only when the user explicitly requests arbitrary keystrokes and understands that it has no occupant fence.

## Treat UI delivery as publication only

A UI result of `delivered=N` counts browser subscribers that received publication. It is not execution acknowledgement and does not prove that any browser tab applied the command. Never report a UI action as applied or successful from `delivered` alone.

## Mutation safety

- Mutate only the explicit session or resource the user requested.
- Inspect immediately before mutating; stale JSON is not authority.
- Do not close, kill, rename, reassign, spawn, or rearrange resources merely to make inspection easier.
- Never broaden from the current session to sibling dashboard resources without user intent.
- Never retry an ambiguous input or infer completion from delivery, terminal text, or silence.

This skill is passive documentation. Installation and updates are manual; never edit agent configuration or install another copy automatically.
