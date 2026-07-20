<!-- AUDIENCE: human -->
<div align="center">

<img src="apps/web/public/icon.svg" width="72" alt="Roost logo">

# Roost

**Built for AI engineers. Run, watch, and steer a fleet of coding agents from one screen, on any device.**

Roost is a tool for AI engineers. Connect your machines to a single coordinator and every coding agent you're running, whether Claude Code, pi, oh-my-pi, or any other terminal tool, lands in one browser tab you can open from a laptop, phone, or tablet. See which agents are working and which are blocked on you, then drop into any of them as a real, full native terminal. At heart it's a terminal multiplexer, retuned for the agentic workflows AI engineers live in. It's self-hosted, so nothing touches a cloud.

_An AI engineer's daily driver. Real infrastructure, not a demo._

[![License](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)
![Runtime](https://img.shields.io/badge/runtime-Bun-14151a?logo=bun&logoColor=white)
![UI](https://img.shields.io/badge/ui-SolidJS-2c4f7c?logo=solid&logoColor=white)

</div>

![Roost, a sidebar of live agents across two Macs next to a working terminal](docs/media/hero.png)

## What it is

Roost is a control room for AI engineers: one screen where you run, observe, and steer a whole fleet of coding agents. Connect each machine to a coordinator and every agent, whether Claude Code, pi, oh-my-pi, or anything else that lives in a terminal, shows up in one browser tab. Sessions are grouped by the machine they run on, each with a live status chip that reads working, waiting on you, or idle.

Under the hood it's a terminal multiplexer, like `tmux`, `screen`, or `zellij`, except the screen is any browser on your network and the panes span every machine you own. Where those tools manage terminals, Roost is built for the way AI engineers actually work. It lifts each agent's state, model, and cost onto the sidebar, keeps every session alive across disconnects, and lets you split, arrange, and switch panes by clicking or tapping. A plain shell is still a first-class session. You just won't need one often, because streamlining agentic workflows is the point.

If you've used `tmux`, `screen`, or `zellij`, you know they live inside one terminal on one machine, driven by a prefix-key grammar, and reaching another box means you SSH in first. Roost turns that inside out. Every machine you own shows up in one place, so you can be connected to ten computers at once without SSH juggling, and the whole surface is built to keep a dozen concurrent agents legible instead of buried in tabs. Still love tmux or zellij? Run them inside a Roost session. Roost just means you no longer have to.

## The problem

An AI engineer runs many coding agents at once, and the moment you have five going across two machines, you lose the thread. The one blocked on a permission prompt is the one you can't find. The heavy run is stuck on the laptop that should have offloaded it. And none of them is reachable once you step away from the desk. Roost puts every agent, on every machine, on one screen you can open from anywhere, and tells you at a glance which one needs you.

## What you get

**Every agent at a glance.** The sidebar shows each session's live state, whether working, waiting for input, or idle, along with its model and cost so far, grouped by the machine it runs on. You don't have to tab through terminals to find the blocked one. State comes from Claude Code's hooks or from screen-scraping the TUI, and the agent's transcript is never consumed to get it.

**Scale your agent fleet across machines.** Connect every Mac you own to one coordinator and run agents on all of them natively, from a single browser tab. One coordinator plus any number of workers that dial outbound only, so there's no inbound port on your other machines. The sidebar groups every live session by the machine it runs on, so ten computers are as easy to work as one, without SSH juggling. You can offload heavy, power-hungry runs onto a beefier box while you keep working on your laptop over the LAN. It works well with exactly one machine too.

![The sidebar: every session grouped by the machine it runs on, with live status and cost](docs/media/sidebar-status.png)

**A full terminal on any device.** The same real terminal renders in Chrome on your desktop and in Safari on your phone, with full ANSI, colors, and scrollback, plus touch selection, an on-screen key row, and gestures. This isn't a companion app or a read-only mirror. You drive the agent from your phone exactly as you would from the terminal it's running in. Add it to your home screen and it installs as a real app (a `standalone` PWA) with its own icon, full-screen, and no browser chrome. I run it daily on iPhone and Android phones and on iPads and Android tablets, and it behaves the way it should everywhere. On an iPad or Android tablet it's a full desktop surface, with the same splits, panes, and shortcuts you use on a laptop rather than a cut-down view.

![Desktop-grade on a tablet, the same real terminal and layout as a laptop](docs/media/tablet-desktop.png)

![A real terminal on a phone, with full ANSI, touch selection, and an on-screen key row](docs/media/mobile-phone.png)

**Open a workspace wherever the work is.** No more `cd`-ing around over SSH to find a project. Browse any machine's folders as a visual grid, drill in, and hit *Open terminal here*. A new workspace starts in that directory, on that machine. Every folder on every computer you own is a couple of clicks away in the same UI.

![A workspace: a tab bar of live sessions above one real terminal](docs/media/workspace-tabs.png)

**Split the screen into as many terminals as you want.** A workspace isn't just one terminal. Split it right or down (⌘D / ⌘⇧D) into a tiled grid of live panes, drag the dividers to resize, or pick an arrange preset (Grid, Columns, Rows, Main + stack, or Equalize) to fill the space evenly. Every pane is a full session, and the layout follows you to every device.

![Multiple terminals tiled in one workspace, auto-arranged to fill the screen](docs/media/split-panes.png)

**Talk to your terminal.** Tap the mic and dictate straight into a session. The transcript is typed in as real input and sent, with no review step in between. It works with zero setup using your browser's built-in speech recognition, though that's a rough fallback. For dictation that's actually good, add a Deepgram API key (Settings → Voice). That's the recommended path, with much higher accuracy and multi-language support, and the key is stored once on the coordinator and shared across every device. It's especially handy on a phone, where typing a long prompt is a chore.

**Sessions that don't die.** PTYs run in a keeper subprocess that outlives worker restarts. Drop WiFi, close the laptop, refresh, or reopen the same session on another machine, and the process is still running with full scrollback. Sessions are event-sourced and the byte stream is resumable from an offset, so reconnects splice back cleanly, with no loss and no duplication.

**The terminal IS the UI.** Your agent's raw TUI renders into a real WASM VT terminal (`@wterm/dom`), with full ANSI and scrollback. There's no chat window to babysit. Status is lifted off the screen without touching the transcript, so what you see is exactly what the agent drew.

**Yours, not a SaaS.** It runs on your hardware over your own network. Auth is an EdDSA JWT minted in the browser with WebCrypto, and the private key lives in IndexedDB and is never sent to the coordinator. There are no shared tokens, no accounts, and no telemetry. Revoke a device by deleting a row.

## How it works

```text
   Browser  (any device on your network)
      │   Connect-RPC over HTTP/2, protobuf binary
      │   terminal bytes · agent events · all state, one connection
      ▼
 ┌─────────────────────────┐
 │ Coordinator  (Bun)      │   event-sourced SQLite · auth · session registry
 │ one machine · port 4102 │   fans live updates out to every open browser
 └───────────┬─────────────┘
             │   raw WebSocket · protobuf frames · worker dials outbound
   ┌─────────┼───────────────────┐
   ▼         ▼                   ▼
 Worker    Worker              Worker        (Bun, one per machine)
 Mac A     Mac B               Mac C
   │  a keeper subprocess hosts every PTY and outlives worker restarts;
   │  a bridge reads each agent's hooks (Claude Code) or screen-scrapes
   │  its TUI (pi, oh-my-pi) to track its state
```

The coordinator handles control and fan-out only. It holds an append-only event log that every session is projected from, so the browser and the server agree on state by replaying the same events rather than mirroring a snapshot. Workers are outbound-only: they dial the coordinator, never the reverse. For the full tour, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Supported agents

Roost tracks live status (working / waiting / idle, plus model and cost) for:

- **Claude Code**, via its `--settings` hooks (`worker claude/hooks.ts`).
- **pi** (`pi-coding-agent`), via terminal screen-scrape (`detect/pi-manifest.ts`); no hooks needed.
- **oh-my-pi** (`@oh-my-pi`), via screen-scrape (`detect/omp-manifest.ts`); no hooks needed.

Anything else that runs in a terminal works as a session too; you just won't get the status chip. That includes other agent CLIs (for example OpenAI Codex CLI, Gemini CLI, Aider, opencode, GitHub Copilot CLI, Cursor CLI) and any plain shell, REPL, or TUI. Adding first-class status tracking for another agent is a small screen-scrape manifest under `worker detect/`.

## Network

Roost talks over whatever network your devices share. It's built and tested on [Tailscale](https://tailscale.com), which gives every device a stable name and connects your phone with no port-forwarding, and that's the recommended path. A Tailscale alternative (WireGuard, Headscale, ZeroTier, and the like) or a plain LAN works too, as long as your browser and the coordinator can reach each other. A few conveniences that resolve a machine's tailnet name, like one-click Screen Sharing and Open in Finder, are Tailscale-specific.

## Install

Roost runs on macOS today. It needs a shared network (Tailscale recommended) plus at least one coding agent (Claude Code, pi, or oh-my-pi) on each Mac you'll run agents on. On the Mac you want as the coordinator:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That installs Bun if needed, gets the code, starts the coordinator and a local worker, and opens Roost in your browser already signed in. There are no tokens to copy.

Two things make growing your fleet painless, with no SSH and no tokens to hand-copy:

- **Add a phone or tablet by QR.** In **Settings → Pair a device**, scan the QR with your phone's camera. It opens Roost and signs itself in, with nothing to type.
- **Add another Mac with one line.** Run `roost add-mac` on the coordinator (or **Settings → Machines → Add machine**) and paste the one-liner it prints on the new Mac. It self-installs and registers over the network, as a pull rather than a push.

The full walkthrough, covering pairing devices and adding machines, is in [`GETTING_STARTED.md`](GETTING_STARTED.md).

![Pair a phone or tablet by scanning a QR; it signs itself in, nothing to type](docs/media/pair-qr.png)

## Roost vs. driving agents from the cloud

Claude on the web and on your phone is a control plane too, but it drives Anthropic-hosted cloud sandboxes, or a single local machine tethered with `claude rc`. You can't reach a *new* terminal on your own computer on demand: away from your desk you're limited to the sandboxes and sessions already running, and only Claude Code runs there. Roost inverts that. It's a control plane **you** own, talking to as many of **your** machines as you like, with no third party in the loop.

|   | Claude on the web / phone | Roost |
|---|---|---|
| **What it drives** | Anthropic-hosted cloud sandboxes, or one local Mac tethered via `claude rc` | Every Mac you own, natively |
| **Open a new terminal while away** | No; limited to sandboxes or sessions already running | Yes; open a fresh terminal in any folder on any machine, from your phone |
| **What runs in it** | Claude Code only | Any agent, shell, REPL, or TUI |
| **Where your code lives** | A cloud VM (or the one tethered Mac) | Your own hardware, your own network |
| **The surface** | A chat window onto the agent | The real terminal, with full ANSI, scrollback, and touch |
| **Hosting** | SaaS, tied to an account | Self-hosted, no account, no telemetry |

Your Mac's browser stays perfectly usable for claude.ai. Roost isn't a replacement for it; it's the piece the cloud can't be, which is native control of your own fleet from anywhere.

## Roadmap

- **Headless servers**, so you can run workers on always-on boxes and not just laptops, and heavy jobs live on the machine that should own them.
- **Windows and Linux.** The worker and coordinator are Bun; the cross-platform desktop shell is scoped (`FEATURES/PLAN-NEUT.md`), and macOS ships first.
- **Multi-user.** The schema already models multiple operators; a UI for it is next.

## Status

Early, and honest about it. I use Roost every day as my primary coding surface, so the paths I hit are solid. Paths I don't may be rough.

- macOS only today (uses macOS PTYs and LaunchAgents)
- Needs a shared network (Tailscale tested) plus at least one supported agent (Claude Code, pi, or oh-my-pi)
- Single-user today; the schema is built for multiple operators but there's no UI for it yet

## Built with

- **Web:** Solid + Vite, `@connectrpc/connect-web`, `@wterm/dom` (WASM terminal core)
- **Coordinator:** Bun, Connect-RPC + protobuf, Kysely + `bun:sqlite`, event-sourced session log, EdDSA-JWT auth
- **Workers:** Bun, native PTYs via `Bun.spawn`, hook + screen-scrape adapters for agent state
- **Transport:** Connect-RPC (browser↔coordinator), protobuf-over-WebSocket (worker↔coordinator)

## Built by

Roost is designed, built, and operated by one AI engineer, [Mihai Mateias](https://github.com/cefege). It isn't a portfolio piece assembled to look good in a repo. It's the tool I run my own coding agents on every day, which is why the hard parts are real and load-bearing: an event-sourced coordinator that projects every session from an append-only log, outbound-only workers that never expose an inbound port, a custom Connect-RPC and protobuf transport, a resumable byte stream that reconnects with no loss and no duplication, a WASM VT terminal that renders full ANSI on a phone, and self-hosted EdDSA-JWT auth with a private key that never leaves the browser.

If you hire AI engineers who ship production systems end to end rather than prototypes, this repository is the resume. Read the code, then read [`ARCHITECTURE.md`](ARCHITECTURE.md).

## License

GPL-3.0-only. See [`LICENSE`](LICENSE).
