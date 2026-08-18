---
competitor: "VibeTunnel"
vendor: "amantus-ai"
license: "MIT"
url: "https://vibetunnel.sh"
order: 2
category: "browser-terminal"
matrix:
  hostPlatforms: "macOS app (Apple Silicon only) or npm on Linux and headless hosts; no Windows (issue #252)"
  clientDevices: "any browser, plus an iOS app its README calls work in progress"
  multiMachine: "no"
  zeroInstallClient: "yes"
  persistentSessions: "partial"
  anyCli: "yes"
  mobileUx: "responsive web interface; the iOS app is declared work in progress and not recommended for production"
  voiceInput: "no"
  pushAgentState: "Session activity indicators showing active or idle; no agent-state push"
  selfHostedNoAccount: "yes"
useInsteadIf: "You want one Mac's terminals in a browser with the least possible setup and don't care about phones, Windows hosts, or fleets."
---

## What VibeTunnel is

VibeTunnel turns any browser into your Mac terminal. It ships as a menu-bar app for Apple Silicon Macs, or as `npm install -g vibetunnel` for Linux, Docker, headless, and Intel-Mac hosts; Windows is not yet supported, tracked as issue #252. Once the server is running you open a dashboard on `http://localhost:4020` and see every forwarded session.

The interaction model is a wrapper command. `vt pnpm run dev`, `vt claude`, or `vt --shell` forwards that terminal into the browser, resolving your shell aliases on the way. Around that it adds session activity indicators for active versus idle, asciinema recording of every session for later playback, Git follow mode that tracks your IDE's branch switching, Cmd/Ctrl+1–9 session switching, several authentication modes including localhost-only, and documented remote access over Tailscale or ngrok. It is MIT licensed. Its own README notes the iOS app is still work in progress and not recommended for production use.

## Where they differ

- **Forwarding from one host versus a control plane over many.** VibeTunnel's server runs on one machine and publishes that machine's terminals. Roost is a coordinator plus N workers across macOS, Linux, and Windows x64, where the coordinator is also a worker. You add a machine with `roost add-machine --platform macos|linux|windows`, which prints a one-shot enrollment command valid for 24 hours; workers then dial outbound only and never expose an inbound port. The sidebar groups sessions by machine with per-machine CPU, memory, disk, and network tiles.

- **Wrapping a command versus opening a terminal anywhere.** With VibeTunnel a session exists because you prefixed a command with `vt` on that host. In Roost you browse folders on any worker from the browser and choose *Open terminal here*, so you can start a brand-new session on a machine you are nowhere near, from a phone.

- **Byte stream versus authoritative cells.** VibeTunnel proxies terminal output to a web terminal. Roost's worker keeps the authoritative cell grid: it rebuilds at one agreed width on resize so history never re-reflows in the browser, stamps every frame with a monotonic `seq` so a stale viewer receives exactly one authoritative full frame instead of duplicated or dropped output, fetches scrollback on demand, and treats double-width CJK and emoji as one atomic two-column span. It forwards mouse tracking only when the running program requested it, and runs mosh-style predictive echo above roughly 10 ms round-trip time, suppressed inside alt-screen TUIs.

- **Responsive web versus a designed phone client.** VibeTunnel gives you a responsive dashboard and an explicitly work-in-progress iOS app. Roost's phone surface is the same full application: touch selection, an on-screen key row with a latching Ctrl, a swipeable deck of terminal cards, a soft keyboard that offsets content rather than reflowing the terminal, and PWA install. Tablets keep the desktop layout, panes, and shortcuts.

- **Activity indicators versus an agent-state model.** VibeTunnel tells you a session is active or idle. Roost distinguishes working, needs input, and done, rolls those up per folder, and can deliver Web Push to a device after you grant permission, so you learn that an agent is blocked without watching the tab. Ten CLIs get first-class status detection and anything else still runs fine, just unlabelled.

- **Things VibeTunnel has that Roost does not.** Asciinema recording of every session, Git follow mode, and a genuinely one-command setup on a Mac. Roost has no session recording and no IDE branch integration.

## Use VibeTunnel instead if…

You want one Mac's terminals in a browser with the least possible setup and don't care about phones, Windows hosts, or fleets. Installing a menu-bar app and typing `vt` in front of a command is a much shorter path than enrolling workers into a coordinator, and if the only thing you need is to glance at a long build or an agent from a laptop on the sofa, that is the right amount of machinery. The asciinema recordings are also a real feature if you want to replay what an agent did.

## Links

- [vibetunnel.sh](https://vibetunnel.sh)
- [github.com/amantus-ai/vibetunnel](https://github.com/amantus-ai/vibetunnel)

See the whole field on the [alternatives hub](/alternatives/), or read how Roost reaches a browser from many machines in [fleet](/docs/fleet/).
