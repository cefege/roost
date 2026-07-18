<!-- AUDIENCE: human -->
<div align="center">

<img src="apps/web/public/icon.svg" width="72" alt="Roost logo">

# Roost

**One place to reach every Mac you own — and drive a real terminal on any of them, from any device.**

Connect all your Macs to a single coordinator, then open, control, and split full native terminals across them from one browser tab — laptop, phone, or tablet — over your own network. Full native control, run anything. It's tuned for coding agents (Claude Code, pi, oh-my-pi), but an agent is optional — a plain shell is a first-class session. Self-hosted; nothing touches a cloud.

[![License](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)
![Runtime](https://img.shields.io/badge/runtime-Bun-14151a?logo=bun&logoColor=white)
![UI](https://img.shields.io/badge/ui-SolidJS-2c4f7c?logo=solid&logoColor=white)

</div>

![Roost — a sidebar of live agents across two Macs next to a working terminal](docs/media/hero.png)

## What it is

Roost is one control plane for every computer you own: connect each Mac to a coordinator and you get full, native terminal control of all of them from a single browser tab. At its core it's a terminal multiplexer — like `tmux` or `screen`, except the screen is any browser on your network and the panes span every machine you own. It's tuned for coding agents: Claude Code, pi, oh-my-pi, and anything else that lives in a terminal. You don't strictly need an agent — a plain shell is a valid session — but agentic workflows are what it's built for.

Set it up once and every terminal on every machine follows you. Reattach the same live session at your desk, from the couch on your phone, or on a tablet on the train — a **real** terminal, full ANSI and scrollback, driven by touch as comfortably as by a keyboard. There's no stripped-down "mobile view": coding agents are terminal-first and UI-second, so Roost perfects the terminal instead of wrapping it in a lossy chat window.

If you've used `tmux`, `screen`, or `zellij`: those live inside one terminal on one machine, driven by a prefix-key grammar, and to reach another box you SSH in first. Roost turns that inside out. Every machine you own shows up in one place — be connected to ten computers at once with no SSH juggling — and you split, arrange, and switch panes by clicking or tapping, so it's approachable even if you've never touched a multiplexer. Still love tmux or zellij? Run them inside a Roost session; Roost just means you no longer have to.

## The problem

Run more than one or two coding-agent sessions and you lose the thread. Five terminal tabs across two Macs, and the one blocked on a permission prompt is the one you can't find — and none of them is reachable when you're away from the desk. Roost puts every session, on every machine, on one screen you can open from anywhere, and tells you at a glance which one needs you.

## What you get

**Many machines, one screen.** Connect every Mac you own to one coordinator and drive all of them — natively — from a single browser tab. One coordinator plus any number of workers that dial outbound only (no inbound port on your other machines); the sidebar groups every live session by the machine it runs on, so ten computers are as easy to work as one, with no SSH juggling. Offload heavy, power-hungry runs onto a beefier box while you keep working on your laptop over the LAN. Works great with exactly one machine too.

![The sidebar: every session grouped by the machine it runs on, with live status and cost](docs/media/sidebar-status.png)

**A full terminal on any device.** The same real terminal renders in Chrome on your desktop and in Safari on your phone — full ANSI, colors, and scrollback, with touch selection, an on-screen key row, and gestures. Not a companion app, not a read-only mirror: you drive the agent from your phone exactly as you would from the terminal it's running in. Add it to your home screen and it installs as a real app (a `standalone` PWA): its own icon, full-screen, no browser chrome. I run it daily on iPhone and Android phones and on iPads and Android tablets — it behaves like it's supposed to, everywhere. On an iPad or Android tablet it's a desktop-grade surface — the same splits, panes, and shortcuts you use on a laptop, production-grade, not a cut-down view.

![Desktop-grade on a tablet — the same real terminal and layout as a laptop](docs/media/tablet-desktop.png)

![A real terminal on a phone — full ANSI, touch selection, and an on-screen key row](docs/media/mobile-phone.png)

**Every agent at a glance.** The sidebar shows each session's live state — working, waiting for input, or idle — with its model and cost-so-far, grouped by the machine it runs on. No tabbing through terminals to find the blocked one. State is read from Claude Code's hooks or by screen-scraping the TUI; the agent's transcript is never consumed to get it.

**Open a workspace wherever the work is.** No `cd`-ing around over SSH to find a project. Browse any machine's folders as a visual grid, drill in, and hit *Open terminal here* — a new workspace starts in that directory, on that machine. Every folder on every computer you own is a couple of clicks away in the same UI.

![A workspace: a tab bar of live sessions above one real terminal](docs/media/workspace-tabs.png)

**Split the screen into as many terminals as you want.** A workspace isn't one terminal. Split it right or down (⌘D / ⌘⇧D) into a tiled grid of live panes, drag the dividers to resize, or hit an arrange preset — Grid, Columns, Rows, Main + stack, or Equalize — to auto-fill the space evenly. Every pane is a full session, and the layout follows you to every device.

![Multiple terminals tiled in one workspace, auto-arranged to fill the screen](docs/media/split-panes.png)

**Talk to your terminal.** Tap the mic and dictate straight into a session — the transcript is typed in as real input and sent, no review step. It works with zero setup using your browser's built-in speech recognition, but that's a rough fallback; for dictation that's actually good, add a Deepgram API key (Settings → Voice) — the recommended path, with far higher accuracy and multi-language support, stored once on the coordinator and shared across every device. Especially handy on a phone, where typing a long prompt is a chore.

**Sessions that don't die.** PTYs run in a keeper subprocess that outlives worker restarts. Drop WiFi, close the laptop, refresh, or reopen the same session on another machine — the process is still running with full scrollback. Sessions are event-sourced and the byte stream is resumable from an offset, so reconnects splice back with no loss and no duplication.

**The terminal IS the UI.** Your agent's raw TUI renders into a real WASM VT terminal (`@wterm/dom`), full ANSI and scrollback. There's no chat window to babysit — status is lifted off the screen without touching the transcript, so what you see is exactly what the agent drew.

**Yours, not a SaaS.** It runs on your hardware over your own network. Auth is an EdDSA JWT minted in the browser with WebCrypto — the private key lives in IndexedDB and is never sent to the coordinator. No shared tokens, no accounts, no telemetry. Revoke a device by deleting a row.

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

The coordinator is control and fan-out only. It holds an append-only event log that every session is projected from, so the browser and the server agree on state by replaying the same events — never by mirroring a snapshot. Workers are outbound-only: they dial the coordinator, never the reverse. For the full tour, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Supported agents

Roost tracks live status (working / waiting / idle, plus model and cost) for:

- **Claude Code** — via its `--settings` hooks (`worker claude/hooks.ts`).
- **pi** (`pi-coding-agent`) — via terminal screen-scrape (`detect/pi-manifest.ts`), no hooks needed.
- **oh-my-pi** (`@oh-my-pi`) — via screen-scrape (`detect/omp-manifest.ts`), no hooks needed.

Anything else that runs in a terminal works as a session too — you just don't get the status chip. That includes other agent CLIs (for example OpenAI Codex CLI, Gemini CLI, Aider, opencode, GitHub Copilot CLI, Cursor CLI) and any plain shell, REPL, or TUI. Adding first-class status tracking for another agent is a small screen-scrape manifest under `worker detect/`.

## Network

Roost talks over whatever network your devices share. It's built and tested on [Tailscale](https://tailscale.com) — it gives every device a stable name and connects your phone with no port-forwarding — and that's the recommended path. A Tailscale alternative (WireGuard, Headscale, ZeroTier, etc.) or a plain LAN works too, as long as your browser and the coordinator can reach each other; some conveniences that resolve a machine's tailnet name (one-click Screen Sharing / Open in Finder) are Tailscale-specific.

## Install

Roost runs on macOS today and needs a shared network (Tailscale recommended) plus at least one coding agent (Claude Code, pi, or oh-my-pi) on each Mac you'll run agents on. On the Mac you want as the coordinator:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That installs Bun if needed, gets the code, starts the coordinator and a local worker, and opens Roost in your browser already signed in. No tokens to copy.

Two things make growing your fleet painless, no SSH and no tokens to hand-copy:

- **Add a phone or tablet by QR.** In **Settings → Pair a device**, scan the QR with your phone's camera — it opens Roost and signs itself in. Nothing to type.
- **Add another Mac with one line.** Run `roost add-mac` on the coordinator (or **Settings → Machines → Add machine**) and paste the one-liner it prints on the new Mac. It self-installs and registers over the network — a pull, not a push.

Full walkthrough — pairing devices, adding machines — is in [`GETTING_STARTED.md`](GETTING_STARTED.md).

![Pair a phone or tablet by scanning a QR — it signs itself in, nothing to type](docs/media/pair-qr.png)

## Roost vs. driving agents from the cloud

Claude on the web and on your phone is a control plane too — but it drives Anthropic-hosted cloud sandboxes, or a single local machine tethered with `claude rc`. You can't reach a *new* terminal on your own computer on demand: away from your desk you're limited to the sandboxes and sessions already running, and only Claude Code runs there. Roost inverts that — it's a control plane **you** own, talking to as many of **your** machines as you own, with no third party in the loop.

|   | Claude on the web / phone | Roost |
|---|---|---|
| **What it drives** | Anthropic-hosted cloud sandboxes, or one local Mac tethered via `claude rc` | Every Mac you own, natively |
| **Open a new terminal while away** | No — limited to sandboxes/sessions already running | Yes — open a fresh terminal in any folder on any machine, from your phone |
| **What runs in it** | Claude Code only | Any agent, shell, REPL, or TUI |
| **Where your code lives** | A cloud VM (or the one tethered Mac) | Your own hardware, your own network |
| **The surface** | A chat window onto the agent | The real terminal — full ANSI, scrollback, touch |
| **Hosting** | SaaS, tied to an account | Self-hosted, no account, no telemetry |

Your Mac's browser stays perfectly usable for claude.ai — Roost isn't a replacement for it, it's the piece the cloud can't be: native control of your own fleet from anywhere.

## Roadmap

- **Headless servers** — run workers on always-on boxes, not just laptops, so heavy jobs live on the machine that should own them.
- **Windows and Linux** — the worker and coordinator are Bun; the cross-platform desktop shell is scoped (`FEATURES/PLAN-NEUT.md`), macOS ships first.
- **Multi-user** — the schema already models multiple operators; a UI for it is next.

## Status

Early, and honest about it. I use Roost every day as my primary coding surface, so the paths I hit are solid. Paths I don't may be rough.

- macOS only today (uses macOS PTYs and LaunchAgents)
- Needs a shared network (Tailscale tested) + at least one supported agent (Claude Code, pi, or oh-my-pi)
- Single-user today; the schema is built for multiple operators but there's no UI for it yet

## Built with

- **Web:** Solid + Vite, `@connectrpc/connect-web`, `@wterm/dom` (WASM terminal core)
- **Coordinator:** Bun, Connect-RPC + protobuf, Kysely + `bun:sqlite`, event-sourced session log, EdDSA-JWT auth
- **Workers:** Bun, native PTYs via `Bun.spawn`, hook + screen-scrape adapters for agent state
- **Transport:** Connect-RPC (browser↔coordinator), protobuf-over-WebSocket (worker↔coordinator)

## License

GPL-3.0-only. See [`LICENSE`](LICENSE).
