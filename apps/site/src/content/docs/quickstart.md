---
title: "Quickstart"
description: "Bring up a coordinator and its first worker with one command, pair a phone by QR, enroll another machine, and open your first workspace."
order: 2
section: "Start"
---

## One command for the first machine

```sh
roost quickstart
```

`quickstart` is the local one-shot installer for a single machine. It checks
Tailscale first and stops if it is missing, then builds the web app, mints a TLS
certificate for the machine's tailnet FQDN, installs the coordinator service,
deploys a worker on the same machine, waits for the coordinator to report
healthy, prints a status readout, and opens your browser already authorized.

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

Two routes reach that origin:

- **Tailscale (default).** Install the Tailscale app on the phone and sign in to
  the same tailnet, then scan.
- **Cloudflare (optional).** After the setup in
  [networking](/docs/networking/), open your Cloudflare hostname on the phone
  with no VPN client, complete the Cloudflare Access login, then pair.

Both routes require Roost pairing. A successful Access login on its own does not
authorize a device.

Three other ways to authorize a browser — pasting a bootstrap token, loopback
self-registration, and tap-to-pair approval — are described in
[security](/docs/security/).

## Add another machine

Extra machines join by **pulling**. There is no SSH from the coordinator and no
token to hand-copy. On the coordinator, use **Settings → Machines → Add machine**
or run one of:

```sh
roost add-machine --platform macos
roost add-machine --platform linux
roost add-machine --platform windows --publisher-sha256 "<trusted publisher certificate SHA-256>"
```

`--label` optionally names the machine up front. Each invocation mints a one-shot
bootstrap token (prefixed `roost_bt_`, valid for 24 hours) and prints the
platform-specific enrollment command.

On a macOS or Linux worker, paste the generated command. It has this shape:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coord>.<tailnet>.ts.net:4102" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

Those two environment variables are the whole contract: `roost join` refuses to
run without both.

For a Windows x64 worker, choose **Windows** in the dialog (or pass
`--publisher-sha256`) and paste the generated command into elevated
PowerShell 5.1+. Before executing anything it downloaded, that command verifies
the release `join.ps1` Authenticode chain, trusted timestamp, and exact
leaf-certificate pin. The signed join script keeps the one-shot token in memory
only while it enrolls the worker and installs the restricted SCM services.

The machine appears in **Settings → Machines** within a few seconds. macOS uses
launchd, Linux uses `systemd --user`, and Windows uses SCM. The token is
single-use and expires 24 hours after minting whether or not it is redeemed.

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

Windows uses a deliberately different set — `Ctrl+Shift+P` for the palette,
`Alt+Shift+D` / `Alt+Shift+S` to split, `Alt+1`–`Alt+9` for tabs, `Alt+Enter`
for spotlight, `Alt+← ↑ → ↓` for pane focus — so that a plain `Ctrl`+letter chord
still reaches the program running in the PTY.

## Confirm it is healthy

```sh
roost status
roost doctor --since 1h
```

`roost status` is the current service, network, and fleet gate: Tailscale state,
coordinator and worker services, coordinator health and tagged SHA, worker
freshness, and whether TLS comes from Tailscale Serve or a direct certificate.
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
