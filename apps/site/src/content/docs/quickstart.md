---
title: "Quickstart"
description: "Bring up a coordinator and its first worker, pair a phone by QR, enroll another macOS or Linux machine, and open your first workspace."
order: 2
section: "Start"
---

## One command for the first machine

Point the coordinator at the origin your front door serves:

```sh
roost quickstart --coordinator-url "https://roost.example.com"
```

That installs the coordinator service on its loopback bind, deploys a worker on
the same machine, waits for health, prints a status readout, and opens your
browser already authorized. The flag is required: Roost never guesses a public
origin. TLS and reachability come from the front door you chose in
[install](/docs/install/).

Coordinator startup owns the self-hosted tenant setup. Before enrollment, it
creates or validates one internal `local@roost.invalid` account, one `personal`
organization, and its `default` dashboard; no separate organization bootstrap
command is required.

That last step uses a one-shot bootstrap token carried in the URL **fragment**
(`#pair=…`). A fragment is never sent to the server, so the token never lands in
the coordinator's logs, in an access log, or in a `Referer` header.

`quickstart` sets up this machine only. Other machines are enrolled separately,
below.

## Pair a phone or tablet

On the machine you just set up, open **Settings → Pair a device**. Roost mints a
one-shot browser token and renders a QR for the current HTTPS origin, again with
the token in the fragment. Scan it with the phone's camera — the phone opens
Roost and signs itself in with nothing to type.

Make the front door reachable from the phone first — on a `tailscale serve`
front door that means installing the Tailscale app on the phone and signing in
to the same tailnet.

Pairing is what authorizes the device. Network reachability, or a login your
front door performs on its own, does not.

Three other ways to authorize a browser — pasting a bootstrap token, loopback
self-registration, and tap-to-pair approval — are described in
[security](/docs/security/).

## Add another machine

`v0.5.0` enrolls macOS and Linux workers. Use **Settings → Machines → Add
machine** to generate a one-shot pull command, or the CLI generators:

```sh
roost add-machine --platform macos
roost add-machine --platform linux
```

`--label` optionally names the machine up front. Each invocation mints a
single-use bootstrap token (prefixed `roost_bt_`, valid for 24 hours) and prints
the enrollment command. Paste that command on the macOS or Linux worker; it has
this shape:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coordinator-host>:<port>" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

Those two environment variables are the enrollment contract. The worker must
reach that origin and trust its certificate chain; `/ws/coord-worker/*` passes
on a standard front door. Give workers a separate origin with
`ROOST_COORDINATOR_PUBLIC_URL` on the coordinator when the worker link should
stay off the public front door — only then is it safe to deny that path at the
edge.

> **Windows host enrollment is paused.** `v0.5.0` publishes no Windows worker,
> package, installer, or signed join script, so there is no Windows command to
> generate or run. Windows remains supported as a browser client.

The machine appears in **Settings → Machines** within a few seconds. macOS uses
launchd and Linux uses `systemd --user`. The token expires 24 hours after
minting whether or not it is redeemed.

## Open your first workspace

You do not `cd` around over SSH to find a project. Pick a machine and browse its
folders as a grid, drill in with a click (or a tap on a phone), and press **Open
terminal here**. A new workspace starts in that directory, on that machine.

Inside the session, run whatever you want: a shell, a REPL, `vim`, `less`, or an
agent CLI such as `omp`, Codex, or Claude Code. Roost never spawns, supervises,
or owns the agent — it is an ordinary command in a real PTY. See
[agents](/docs/agents/) for which CLIs also get a status badge.

## Split panes and move around

A workspace is not limited to one terminal. Drag a tab onto the edge of a pane to
split right, left, up, or down, drag the dividers to resize, or use **Arrange**
for a preset: Grid, Columns, Rows, Main + stack, or Equalize sizes.

On macOS and Linux keyboards:

| Action | Binding |
|---|---|
| Command palette / open terminal | `⌘K` |
| Filter the sidebar | `⌘F` |
| Toggle the sidebar | `⌘B` |
| Split right / split down | `⌘D` / `⌘⇧D` |
| Arrange grid / columns / rows / main+stack / equalize | `⌘⌥G` / `E` / `R` / `V` / `B` |
| Focus tabs 1–8 / last tab in the pane | `⌘1`–`⌘8` / `⌘9` |
| Move focus to the adjacent pane | `⌘⌥← ↑ → ↓` |
| New terminal in the focused pane | `⌘⌥T` |
| Bring the pane to front / push back | `⌘↵` |
| Settings | `⌘,` |
| Shortcut help | `Shift+?` |

When browsing from Windows, Roost uses a deliberately different shortcut set —
`Ctrl+Shift+P` for the palette, `Alt+Shift+D` / `Alt+Shift+S` to split,
`Alt+1`–`Alt+9` for tabs, `Alt+Enter` for spotlight, and `Alt+← ↑ → ↓` for pane
focus — so that a plain `Ctrl`+letter chord still reaches the program running in
the PTY.

## Confirm it is healthy

```sh
roost status
roost doctor --since 1h
```

`roost status` is the current service, network, and fleet gate: it reports both
local services, coordinator health and tagged SHA on its loopback bind, the
configured public URL and whether it answers, and worker freshness.
`roost doctor --since <window>` is a different question — it summarizes the local
logs from that window and reports anomaly counts such as uncaught errors,
sequence gaps, queue overflows, degraded keepers, and failed backups or
readiness events. The default window is 24h; the flag accepts forms like `90m`,
`1h`, or `7d`.

Logs themselves:

```sh
roost logs coord
roost logs worker --tail 500
```

## Next

- [Fleet](/docs/fleet/) — coordinator, workers, keepers, and fleet updates
- [The terminal](/docs/terminal/) — why sessions survive resize and reconnect
- [Mobile](/docs/mobile/) — the phone and tablet client
- [The CLI](/docs/cli/) — every subcommand and `roost api` verb
