<!-- AUDIENCE: human -->
<div align="center">

<img src="apps/web/public/icon.svg" width="72" alt="Roost logo">

# Roost

**Run all your coding agents from one browser tab — and always know which one needs you.**

A self-hosted control plane for Claude Code, pi, and oh-my-pi across every Mac you own, over your own Tailscale network. Nothing touches a cloud.

[![License](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)
![Runtime](https://img.shields.io/badge/runtime-Bun-14151a?logo=bun&logoColor=white)
![UI](https://img.shields.io/badge/ui-SolidJS-2c4f7c?logo=solid&logoColor=white)

</div>

![Roost — sidebar of live agents next to a working terminal](docs/media/hero.png)

## The problem

Run more than one or two coding-agent sessions and you lose the thread. Five terminal tabs across two Macs, and the one that's blocked on a permission prompt is the one you can't find. Roost puts every session on one screen and tells you, at a glance, which one needs you.

## What you get

**Every agent at a glance.** The sidebar shows each session's live state — working, waiting for input, or idle — with its model and cost-so-far. No more tabbing through terminals to find the blocked one. State is extracted from Claude Code's hooks or by screen-scraping the TUI; the agent's transcript is never consumed to get it.

![The sidebar: each session's live state and machine, at a glance](docs/media/sidebar-status.png)

**Sessions that don't die.** PTYs run in a keeper subprocess that outlives worker restarts. Drop WiFi, close the laptop, refresh, or reopen the same session on another Mac — the process is still running with full scrollback. Sessions are event-sourced and the byte stream is resumable from an offset, so reconnects splice back with no loss and no duplication.

**Multi-Mac, one screen.** One coordinator, any number of workers that dial outbound only — no inbound port on your other Macs. Drive all of them from a single tab. Works great with exactly one Mac too.

![A workspace: a tab bar of live sessions above one real terminal](docs/media/workspace-tabs.png)

**The terminal IS the UI.** Your agent's raw TUI renders into a real WASM VT terminal (`@wterm/dom`), full ANSI and scrollback. There's no chat window to babysit — status is lifted off the screen without touching the transcript, so what you see is exactly what the agent drew.

**Yours, not a SaaS.** It runs on your hardware over your tailnet. Auth is an EdDSA JWT minted in the browser with WebCrypto — the private key lives in IndexedDB and is never sent to the coordinator. No shared tokens, no accounts, no telemetry. Revoke a device by deleting a row.

## How it works

```text
   Browser  (any device on your tailnet)
      │   Connect-RPC over HTTP/2, protobuf binary
      │   terminal bytes · agent events · all state, one connection
      ▼
 ┌─────────────────────────┐
 │ Coordinator  (Bun)      │   event-sourced SQLite · auth · session registry
 │ one Mac · port 4102     │   fans live updates out to every open browser
 └───────────┬─────────────┘
             │   raw WebSocket · protobuf frames · over Tailscale
   ┌─────────┼───────────────────┐
   ▼         ▼                   ▼
 Worker    Worker              Worker        (Bun, one per Mac)
 Mac A     Mac B               Mac C
   │  a keeper subprocess hosts every PTY and outlives worker restarts;
   │  a bridge reads each agent's hooks (Claude Code) or screen-scrapes
   │  its TUI (pi, oh-my-pi) to track its state
```

The coordinator is control and fan-out only. It holds an append-only event log that every session is projected from, so the browser and the server agree on state by replaying the same events — never by mirroring a snapshot. Workers are outbound-only: they dial the coordinator, never the reverse. For the full tour, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Supported agents

- **Claude Code** — tracked via its `--settings` hooks (`worker claude/hooks.ts`).
- **pi** (`pi-coding-agent`) — tracked by terminal screen-scrape (`detect/pi-manifest.ts`), no hooks needed.
- **oh-my-pi** (`@oh-my-pi`) — screen-scrape (`detect/omp-manifest.ts`), no hooks needed.

Any other terminal app or plain shell works as a session; you just lose the status chip.

## Install

Roost needs [Tailscale](https://tailscale.com) running (it's the network everything talks over) and at least one coding agent (Claude Code, pi, or oh-my-pi) on each Mac you'll run agents on. Then, on the Mac you want as the coordinator:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That installs Bun if needed, gets the code, starts the coordinator and a local worker, and opens Roost in your browser already signed in. No tokens to copy.

Full walkthrough — pairing your phone, adding more Macs — is in [`GETTING_STARTED.md`](GETTING_STARTED.md). Adding another Mac is a pull, not a push: run `roost add-mac` on the coordinator (or **Settings → Machines → Add machine**) and paste the one-liner it prints on the new Mac. It self-installs and registers over the tailnet, no SSH.

## How Roost differs

It's the inverse of Anthropic's Remote Control, which tethers a single Mac to claude.ai with a short timeout. Roost is a control plane you own, talking to as many Macs as you own, with no third party in the loop.

## Status

Early, and honest about it. I use Roost every day as my primary coding surface, so the paths I hit are solid. Paths I don't may be rough.

- macOS only (uses macOS PTYs and LaunchAgents)
- Requires Tailscale + at least one supported agent (Claude Code, pi, or oh-my-pi)
- Single-user today; the schema is built for multiple operators but there's no UI for it yet

## Built with

- **Web:** Solid + Vite, `@connectrpc/connect-web`, `@wterm/dom` (WASM terminal core)
- **Coordinator:** Bun, Connect-RPC + protobuf, Kysely + `bun:sqlite`, event-sourced session log, EdDSA-JWT auth
- **Workers:** Bun, native PTYs via `Bun.spawn`, hook + screen-scrape adapters for agent state
- **Transport:** Connect-RPC (browser↔coordinator), protobuf-over-WebSocket (worker↔coordinator), all over Tailscale

## License

GPL-3.0-only. See [`LICENSE`](LICENSE).
