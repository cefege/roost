<!-- AUDIENCE: human -->
<div align="center">

<img src="apps/web/public/icon.svg" width="72" alt="Roost logo">

# Roost

**One control panel for every machine you own.**

Roost is a self-hosted, Bun-powered terminal control plane. One machine hosts the coordinator and works as a worker itself; add every other macOS, Linux, or Windows x64 machine as a worker. Your main laptop and spare machines become one terminal fleet you can control from a laptop, tablet, or phone.

**macOS, Linux, and Windows x64 can run Roost coordinators and workers.** You reach them from a browser, so the device in your hand can be anything: Mac, Windows, Linux, iPhone, Android, iPad, or Android tablet.

_I built Roost for my own daily workflow: real infrastructure, not a demo._

[![License](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
![Servers](https://img.shields.io/badge/coordinator%20%2B%20workers-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
![Clients](https://img.shields.io/badge/clients-any%20browser%20%C2%B7%20iOS%20%7C%20Android%20%7C%20desktop-lightgrey)
![Runtime](https://img.shields.io/badge/runtime-Bun-14151a?logo=bun&logoColor=white)
![UI](https://img.shields.io/badge/ui-SolidJS-2c4f7c?logo=solid&logoColor=white)

</div>


## What it is

One coordinator connects every machine you own into one fleet, and the coordinator machine can be a worker too. Add spare or older MacBooks, a Mac mini, a Linux box, or a Windows workstation, spread agent workloads across them, and manage every session from the same control panel. Pick a folder on any machine, open a workspace there, and the process runs on that machine while its native terminal stays available on every device you use — including devices that could never host a worker, like a phone or tablet.

Every session is a shell PTY rendered in the browser. Launch any agent CLI, shell, REPL, or TUI inside it and use that program's native terminal interface. Roost never spawns, supervises, or owns an agent session; the CLI remains an ordinary command in its shell PTY.

## The problem

Once you have agents across several machines, spare hardware often sits unused while the main laptop carries the CPU and RAM load. "Remote" means SSH, separate terminals, and hunting for the session that needs you. Roost turns those machines into workers, puts every session in one place, and lets you put the work on the machine that has capacity.

## Three ways I use it

**Leave the heavy work at home.** Have old MacBooks, a spare Mac mini, a Linux box, or a Windows workstation sitting around? Add them as workers, choose the project folder on each one, and put the heavy agent runs there. Their CPU and RAM carry the workload while your main laptop stays light enough to take with you. Roost keeps all of those sessions in the same control panel, so working from a different machine does not mean juggling a different workflow.

**Take only a tablet.** Roost is a fully functional web app, not a cut-down remote viewer. Pair an iPad or Android tablet, add a keyboard, and you get the same terminal, pane layout, and shortcuts as the desktop surface. The work still runs on your workers at home; the tablet is simply another way into the same sessions.

**Only have your phone?** The same fully functional web app runs on iPhone and Android. Start a terminal in a worker's folder, select terminal text by touch, use the on-screen key row, attach a file, or dictate a prompt with the microphone. The phone UI uses Material 3 patterns, including swipeable terminal cards and touch gestures, so it remains practical when the phone is the only device you have.

## What you get

**Run any terminal program.** Pick a server and folder, open a terminal, and launch `omp`, Claude Code, Codex, a shell, a REPL, or any other TUI. Roost keeps the PTY alive and renders the program's own terminal interface on every device.

**Build a fleet from the machines you already own.** Connect every macOS, Linux, and Windows x64 machine to one coordinator, then use each one as a worker, including the coordinator machine. Put an agent run where CPU and RAM are available, pick a project folder on that worker, and open a workspace there. Workers dial outbound only, so they do not expose an inbound port. The sidebar groups every live session by machine, so the entire fleet stays legible in one browser tab.


**A full terminal on every device.** The same fully functional web app runs on any modern browser — macOS, Windows, and Linux desktops, iPhone, Android phones, iPads, and Android tablets. Nothing to install on the device you browse from. It renders the real persistent PTY with full ANSI, colors, and scrollback. Upload files into a session, download files from a worker to your browser, and use the terminal without caring which machine runs it. Tablets keep the desktop layout, panes, and shortcuts. Phones add touch selection, an on-screen key row, and gestures for real work. Add it to your home screen and it installs as a standalone PWA with its own icon and no browser chrome.

![Desktop-grade on a tablet, the same real terminal and layout as a laptop](docs/media/tablet-desktop.png)

![A real terminal on a phone, with full ANSI, touch selection, and an on-screen key row](docs/media/mobile-phone.png)

**Open a workspace wherever the work is.** No more `cd`-ing around over SSH to find a project. Browse any machine's folders as a visual grid, drill in, and hit *Open terminal here*. A new workspace starts in that directory, on that machine. Every folder on every computer you own is a couple of clicks away in the same UI.

![A workspace: a tab bar of live sessions above one real terminal](docs/media/workspace-tabs.png)

**Split the screen into as many terminals as you want.** A workspace isn't just one terminal. Drag a tab onto the edge of a pane to split right, left, up, or down, drag the dividers to resize, or hit the Arrange button for a preset (Grid, Columns, Rows, Main + stack, or Equalize) that fills the space evenly. On a Mac keyboard there are accelerators for all of it — ⌘D / ⌘⇧D to split the focused pane, ⌘⌥G/E/R/V/B for the presets — and ⌃1–9, ⌃⌥arrows, and ⌃⌥T work the same on Ctrl keyboards. Every pane is a full session, and the layout follows you to every device.

![Multiple terminals tiled in one workspace, auto-arranged to fill the screen](docs/media/split-panes.png)

**Talk to your terminal.** Tap the mic and dictate straight into a session. The recognized text is typed in as real input and sent, with no review step in between. It works with zero setup using your browser's built-in speech recognition, though that's a rough fallback. For dictation that's actually good, add a Deepgram API key (Settings → Voice). That's the recommended path, with much higher accuracy and multi-language support, and the key is stored once on the coordinator and shared across every device. It's especially handy on a phone, where typing a long prompt is a chore.

**Sessions that don't die.** PTYs run in a keeper subprocess that outlives worker restarts. Drop WiFi, close the laptop, refresh, or reopen the same session on another machine, and the process is still running with full scrollback. Sessions are event-sourced and the byte stream is resumable from an offset, so reconnects splice back cleanly, with no loss and no duplication.


**See which agent needs you.** Every terminal running a coding agent carries one
state — working, needs input, or done — on its sidebar row, tab, mobile card,
and folder rollup (`2 working · 1 needs input`). Plain shells stay unmarked.
OMP and Pi report their own lifecycle, including "waiting on you"; other agents
are detected from what their terminal shows. When a background agent stops for
input or finishes, you get a toast, an unseen count in the tab title, and —
after you grant permission in Settings → Notifications — a real OS notification
on your phone or laptop that opens straight to that session. Nothing about
status is stored: restart anything and it re-derives itself.

**The terminal is the only interactive surface.** Every session owns a PTY. Output renders in a real WASM VT terminal (`@wterm/dom`) with full ANSI and scrollback, whether the process is an agent CLI, shell, REPL, editor, or other TUI.

**Yours, not a SaaS.** It runs on your hardware over your own network. Auth is an EdDSA JWT minted in the browser with WebCrypto, and the private key lives in IndexedDB and is never sent to the coordinator. There are no shared tokens, no accounts, and no telemetry. Revoke a device by deleting a row.

## How it works

```text
   Browser  (any device on your network)
      │   Connect-RPC over HTTP/2, protobuf binary
      │   terminal data · session state, one connection
      ▼
 ┌─────────────────────────┐
 │ Coordinator  (Bun)      │   event-sourced SQLite · auth · session registry
 │ one machine · port 4102 │   fans live updates out to every open browser
 └───────────┬─────────────┘
             │   raw WebSocket · protobuf frames · worker dials outbound
   ┌─────────┼───────────────────┐
   ▼         ▼                   ▼
 Worker    Worker              Worker        (Bun, one per machine)
 macOS     Linux               Windows
   │  a keeper subprocess hosts every PTY and outlives worker restarts
```

The coordinator handles control and fan-out only. It holds an append-only event log that every session is projected from, so the browser and the server agree on state by replaying the same events rather than mirroring a snapshot. Workers are outbound-only: they dial the coordinator, never the reverse. For the full tour, see [`ARCHITECTURE.md`](ARCHITECTURE.md).


## Network

Roost's supported automated production topology is [Tailscale](https://tailscale.com): it gives every device a stable name, provides the trusted private enrollment boundary, and connects phones without port-forwarding. WireGuard, Headscale, ZeroTier, other VPNs, and a plain LAN can be wired up manually when browser and coordinator reachability is already solved, but those paths are not exercised by the installer or release canaries.

**[Cloudflare browser access](GETTING_STARTED.md#optional-browser-access-through-cloudflare) is optional.** Browsers may enter through Cloudflare Access and Tunnel, while the coordinator and every worker still communicate over Tailscale. A phone, tablet, or browser-only computer using the Cloudflare hostname needs only an ordinary browser, not Tailscale; worker machines still need Tailscale. That's the setup for reaching your fleet from a device you can't or won't install a VPN client on — a work laptop, a borrowed machine, a locked-down phone. Only the coordinator runs `cloudflared` (macOS via Homebrew, Linux via Cloudflare's apt/rpm repo). This removes the client-install requirement from browser devices without changing worker enrollment, worker WebSockets, `roost deploy`, `roost status`, direct-machine links, or coordinator relocation. Run `roost expose` only as a post-install step after Roost is already working over Tailscale.

## Install

Roost's coordinator and workers run on macOS, Linux, and Windows x64. POSIX hosts use launchd or `systemd --user`; Windows uses restricted SCM services. Browsing devices need only a modern browser. The supported production install uses Tailscale.

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
"$HOME/.local/bin/roost" quickstart
```

On Windows x64, install Tailscale first and use the signed PowerShell 5.1+
bootstrap flow in [`GETTING_STARTED.md`](GETTING_STARTED.md#install--run). It
requires the release-publisher certificate SHA-256 from an independent trusted
channel, verifies the downloaded installer's Authenticode chain, trusted
timestamp, and exact leaf-certificate pin before execution, then verifies the
signed release manifest and every package file.

For source development only, `curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash` installs Bun and a checkout that tracks `main`; it is not the pinned production release path.

Two things make growing your fleet painless, with no SSH and no tokens to hand-copy:

- **Add a phone or tablet by QR.** In **Settings → Pair a device**, scan the QR with your phone's camera. It opens Roost and signs itself in, with nothing to type.
- **Add another machine.** On the coordinator, run `roost add-machine --platform macos`, `roost add-machine --platform linux`, or `roost add-machine --platform windows` (or use **Settings → Machines → Add machine**), then paste its one-shot enrollment command on the new host.

The full walkthrough, covering pairing devices and adding machines, is in [`GETTING_STARTED.md`](GETTING_STARTED.md).

![Pair a phone or tablet by scanning a QR; it signs itself in, nothing to type](docs/media/pair-qr.png)

## Roost vs. driving agents from the cloud

Claude on the web and on your phone is a control plane too, but it drives Anthropic-hosted cloud sandboxes, or a single local machine tethered with `claude rc`. You can't reach a *new* terminal on your own computer on demand: away from your desk you're limited to the sandboxes and sessions already running, and only Claude Code runs there. Roost inverts that. It's a control plane **you** own, talking to as many of **your** machines as you like, with no third party in the loop.

|   | Claude on the web / phone | Roost |
|---|---|---|
| **What it drives** | Anthropic-hosted cloud sandboxes, or one local Mac tethered via `claude rc` | Every macOS, Linux, or Windows x64 machine you own, natively |
| **Open a new terminal while away** | No; limited to sandboxes or sessions already running | Yes; open a fresh terminal in any folder on any machine, from your phone |
| **What runs in it** | Claude Code only | Any agent, shell, REPL, or TUI |
| **Where your code lives** | A cloud VM (or the one tethered Mac) | Your own hardware, your own network |
| **The surface** | A chat window onto the agent | The real terminal, with full ANSI, scrollback, and touch |
| **Hosting** | SaaS, tied to an account | Self-hosted, no account, no telemetry |

Your desktop browser stays perfectly usable for claude.ai. Roost isn't a replacement for it; it's the piece the cloud can't be, which is native control of your own fleet from anywhere.

## Roadmap

- **Headless-server polish.** Linux workers already install as a `systemd --user` unit with linger, so an always-on box keeps running after logout; the remaining work is the setup path for a box you only ever reach over SSH.
- **Multi-user.** The schema already models multiple operators; a UI for it is next.

## Status

Early, and honest about it. I use Roost every day as my primary coding surface, so the paths I hit are solid. Paths I don't may be rough.

- Coordinator and workers: macOS, Linux, and Windows x64 (launchd, `systemd --user`, or Windows SCM services)
- Browsing devices: any modern browser — macOS, Windows, Linux, iOS, iPadOS, Android — with nothing to install
- Needs a shared network (Tailscale tested) and whatever terminal CLI tools you want to run
- Single-user today; the schema is built for multiple operators but there's no UI for it yet

## Built with

- **Web:** Solid + Vite, `@connectrpc/connect-web`, `@wterm/dom` (WASM terminal core)
- **Coordinator:** Bun, Connect-RPC + protobuf, Kysely + `bun:sqlite`, event-sourced session log, EdDSA-JWT auth
- **Workers:** Bun, native PTYs via `Bun.spawn`
- **Transport:** Connect-RPC (browser↔coordinator), protobuf-over-WebSocket (worker↔coordinator)

## Built by

I'm Mihai — I build and run Roost solo, and it's my daily coding surface. That's why the hard parts are real rather than demo-deep: an event-sourced coordinator that projects every session from an append-only log, outbound-only workers that never expose an inbound port, a custom Connect-RPC and protobuf transport, a resumable byte stream that reconnects with no loss and no duplication, a WASM VT terminal that renders full ANSI on a phone, and self-hosted EdDSA-JWT auth with a private key that never leaves the browser.

Reach me on [GitHub](https://github.com/cefege) or [LinkedIn](https://de.linkedin.com/in/mihai-mateias).

## License

GPL-3.0-only. See [`LICENSE`](LICENSE).
