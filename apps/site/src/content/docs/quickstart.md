---
title: "Quickstart"
description: "Bring up a coordinator and its first worker, pair a phone by QR, enroll another macOS or Linux machine, and open your first workspace."
order: 2
section: "Start"
---

## One command for the first machine

```sh
roost quickstart
```

On macOS or Linux this installs persistent local coordinator and worker
services, waits for health, and opens an already-authorized browser at
`http://127.0.0.1:4103`. The local profile is
`ROOST_COORDINATOR_BIND=127.0.0.1:4103`, `ROOST_TRUST_PROXY=0`,
`ROOST_WEB_PUBLIC_URL=`, `ROOST_COORDINATOR_PUBLIC_URL=`,
`ROOST_CORS_ALLOWED_ORIGINS=http://127.0.0.1:4103`, and
`ROOST_SKIP_ENV_LOCAL=1`. No domain, HTTPS proxy, VPN, or external URL is
needed for the first machine. Remote access is optional: when you need it,
you choose and operate the HTTPS front door.

The browser receives a one-shot bootstrap token in the URL **fragment**
(`#pair=…`). A fragment is never sent to the server, so the token never lands
in the coordinator's logs, an access log, or a `Referer` header. Redeeming that
scoped token authorizes the browser; local network position does not.

Rerun `roost quickstart` to reactivate the installed coordinator and reopen a
browser pairing flow. It preserves the installed endpoint, worker, keeper, and
state.

To make Roost available beyond the coordinator host, first choose an
operator-managed HTTPS address, then run this on the coordinator machine
**before** exposing its listener:

```sh
roost quickstart --coordinator-url "https://roost.example.com"
```

Promotion changes the coordinator endpoint profile and restarts only the
coordinator. Configure the front door to forward to the installed loopback bind
and overwrite `X-Forwarded-For`; Roost does not configure TLS, a proxy, VPN,
SSH, or target reachability. See [networking](/docs/networking/) for the
private-path and proxy requirements.

Coordinator startup owns the self-hosted tenant setup. Before enrollment, it
creates or validates one internal `local@roost.invalid` account, one `personal`
organization, and its `default` dashboard; no separate organization bootstrap
command is required.

`quickstart` sets up this machine only. Other machines are enrolled separately,
below.

## Pair a phone or tablet

Phone pairing requires an HTTPS front door the phone can reach. After promotion
and front-door configuration, open that HTTPS origin, then choose **Settings →
Pair a device** in Roost on an already-authorized browser. Roost renders a QR
with a one-shot fragment token; scan it with the phone's camera. On a
`tailscale serve` front door, install the Tailscale app on the phone and sign
in to the same tailnet.

Pairing is what authorizes the device. Network reachability, or a login your
front door performs on its own, does not.

### Confirm a browser-pairing request

An unpaired browser shows only the **Pair this browser** page, whatever URL it
opens. Choose **Request approval**. In an already-authorized browser, approve
the request and read the displayed six-digit code to the requester. Approval
alone grants nothing: the requester must enter the matching code before it
becomes authorized. The approver's code window closes by itself once the
requester confirms, and every open paired browser shows **New browser paired**.
Closing the code window early cancels the request. The request ID, requester
token, and approver code stay tab-local and never enter a URL. If a tab says
`pairing client must reload`, reload and start again; the version-1 upgrade
expires legacy pending requests rather than letting an old ceremony continue.

A one-time setup token (`roost_bt_…`) and recovery for a revoked browser key
live under **Other pairing options** on the same page.

## Add another machine

`v0.5.0` enrolls macOS and Linux workers. Open **Settings → Machines → Add
machine**. For a local-only coordinator, the dialog explains that the new
machine needs a reachable HTTPS address and does not mint a token.

1. Choose an HTTPS address from the operator's proxy, tunnel, or
   private-access setup. No purchased domain is required; Tailscale Serve can
   supply a `*.ts.net` address, but network membership alone does not expose
   Roost.
2. Before exposing the local listener, run
   `roost quickstart --coordinator-url https://<your-Roost-address>` on the
   coordinator machine.
3. Configure the front door to forward to the installed loopback bind and
   overwrite `X-Forwarded-For`. The default-port Tailscale Serve example is
   `tailscale serve --bg --https=443 http://127.0.0.1:4103`; substitute an
   operator-changed loopback port. Both machines need the appropriate route and
   ACL access; WireGuard alone does not supply HTTPS.
4. Return to **Add machine** and select **Check again**. Generate a command
   only after the coordinator advertises a valid external origin.

Run the generated command manually on the intended target using its normal
terminal, SSH session, or cloud console. Roost does not log in through SSH or
configure the network, and generation does not prove the target can reach the
address. The target must reach the declared HTTPS origin and trust its
certificate chain.

The CLI generators are:

```sh
roost add-machine --platform macos
roost add-machine --platform linux
```

`--label` optionally names the machine up front. Each generated command has a
single-use bootstrap token (prefixed `roost_bt_`, valid for 24 hours):

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coordinator-host>:<port>" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

The front door must accept `/ws/coord-worker/*` when workers use the browser
door. A separate `ROOST_COORDINATOR_PUBLIC_URL` is for a distinct worker HTTPS
door only.

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
local services, coordinator health and tagged SHA on its loopback bind, and
worker freshness. An absent external URL is the healthy intended local-only
state; a configured URL that does not answer needs front-door repair.
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
