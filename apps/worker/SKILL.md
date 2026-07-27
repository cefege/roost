---
name: roost
description: "Control sibling Roost sessions from inside one. List, read, send-text, send-keys, and wait on output across PTYs on the same worker. Use when running inside an Roost-spawned session (ROOST_ENV=1)."
---

# roost — agent skill

Before using this skill, check that `ROOST_ENV=1` is set in your
environment. If it is not, you are not running inside an Roost-spawned
PTY — stop and explain that you cannot drive sibling sessions from
outside the multiplexer.

When inside an Roost session you have these environment variables:

- `ROOST_ENV=1` — sentinel.
- `ROOST_SOCKET=<path>` — the worker-local JSON-RPC socket.
- `ROOST_SESSION_ID=<n>` — your own session id (so you can avoid
  driving yourself).
- `ROOST_WORKER_UID=<uid>` — the worker you're attached to.

The `roost` CLI is on your `$PATH`. It connects to `$ROOST_SOCKET` and
prints JSON (except `roost read`, which prints raw text). Exit code is
0 on success, 2 on protocol error.

## Concepts

- **Session.** One shell PTY on this worker. Each session has a numeric
  `session_id` and a working `folder`.
- **Worker.** One Mac. The skill API is worker-local — you control
  sibling sessions on the same Mac. Cross-machine orchestration goes
  through the desktop today.

## Commands

### List sessions on this worker

```bash
roost list
```

Prints JSON `{"sessions":[{"session_id":N,"folder":...,"kind":...,
"exit_code":null|N,"bytes_total":N}, ...]}`.

### Read recent output from a session

```bash
roost read 5
roost read 5 --lines 40
```

Prints the ring-buffer snapshot as raw text. Use `--lines N` to tail.
Output is the same bytes Roost displays in its terminal pane.

### Send text into a session (no Enter)

```bash
roost send-text 5 "git status"
```

The bytes go straight into the PTY input. Useful for filling prompts
or typing into a TUI.

### Send a named key

```bash
roost send-keys 5 Enter
roost send-keys 5 C-c
roost send-keys 5 Up
```

Known names: `Enter`, `Return`, `Tab`, `Escape` / `Esc`, `Space`,
`Backspace`, `Delete`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`,
`PageUp`, `PageDown`, and `C-x` ctrl chords (`C-c`, `C-d`, `C-l`, ...).

### Run a command (text + Enter in one atomic write)

```bash
roost run 5 "cargo test"
```

This is the common case. Equivalent to `send-text "<cmd>"` followed by
`send-keys Enter` but sent as a single PTY write — no race between
the two frames.

### Wait until a session's output matches

```bash
roost wait 5 --substring "test result" --timeout-ms 60000
roost wait 5 --regex "^ok|FAIL"
```

Returns when a line in the session's output contains the substring
or matches the regex. The default timeout is 30 seconds. Exit code 2
on timeout — branch on that to gracefully handle hangs.

## Recipes

### Run tests in a sibling and wait for the result

```bash
roost run 5 "cargo test --quiet"
roost wait 5 --substring "test result" --timeout-ms 120000
roost read 5 --lines 20
```

### Watch what another agent is doing without disturbing it

```bash
roost list
roost read 7 --lines 80
```

You see exactly what the user sees in that pane.

### Drive a TUI prompt step by step

```bash
roost send-text 5 "y"
roost send-keys 5 Enter
roost wait 5 --substring "Confirm again"
roost send-keys 5 Enter
```

## Notes

- IDs can change as sessions are killed and re-spawned. Re-run
  `roost list` when you need a fresh id.
- Don't drive your own pane (`$ROOST_SESSION_ID`) — it would race
  with the user's input.
- The skill API is worker-local. To act on a session on another Mac,
  ask the user to focus that Mac in the desktop app, then run the
  skill there.
