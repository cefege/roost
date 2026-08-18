---
competitor: "amux"
vendor: "Mixpeek"
license: "MIT"
url: "https://amux.io"
order: 4
category: "agent-terminal"
matrix:
  hostPlatforms: "macOS, Linux, and cloud VMs"
  clientDevices: "any browser on the local HTTPS dashboard, a PWA, and a native iOS app"
  multiMachine: "no"
  zeroInstallClient: "yes"
  persistentSessions: "yes"
  anyCli: "partial"
  mobileUx: "PWA for iOS and Android plus a native iOS app on the App Store"
  voiceInput: "no"
  pushAgentState: "Live session status, token spend, and terminal peek in the dashboard and iOS app"
  selfHostedNoAccount: "yes"
useInsteadIf: "You want unattended overnight fleets of agents with auto-restart, cost accounting, and a kanban board on a single host."
---

## What amux is

amux, by Mixpeek, is an MIT-licensed control plane for parallel coding agents: a single Rust binary that drives tmux sessions and serves a local HTTPS dashboard at `https://localhost:8824`. You register a project, start it, and run `amux serve`; it targets 5 to 50 concurrent sessions on macOS, Linux, or a cloud VM, with official support for Claude Code, Codex CLI, and Gemini CLI.

Its centre of gravity is unattended operation. A self-healing watchdog auto-compacts context, restarts crashed sessions, and replays the last message, so a fleet left running overnight is still working in the morning. A SQLite kanban board hands out tasks with atomic claiming so two agents cannot pick up the same work, a REST API plus channels let sessions message each other and peek at each other's output, shared notes give agents memory across sessions, and the dashboard tracks token spend per session. On top of that it ships a PWA and a native iOS app for monitoring and recovering sessions from a phone, plus a cloud tunnel for a stable public URL.

## Where they differ

amux is the closest thing in the field to Roost on the "web dashboard plus mobile" axis, so the differences are worth being precise about.

- **One host versus a fleet of hosts.** amux runs on the machine where the agents run; the dashboard is that machine's dashboard. Roost separates the control plane from the compute: one coordinator, N workers across macOS, Linux, and Windows x64, the coordinator itself also a worker, and a sidebar that groups every live session by machine with per-machine CPU, memory, disk, and network tiles. Workers dial outbound only and never expose an inbound port, which is what makes a laptop, a desktop, and a VPS usable as one pool.

- **tmux-backed sessions versus PTYs the control plane owns.** amux drives tmux. Roost's keeper subprocess hosts every PTY directly and outlives worker restarts and updates, and the worker keeps an authoritative cell grid: rebuilt at one agreed width on resize so history never re-reflows, a monotonic `seq` on every frame so a reconnecting viewer gets exactly one authoritative full frame, on-demand scrollback, atomic two-column spans for double-width CJK and emoji, mouse tracking forwarded only when the program asked for it, and predictive echo above roughly 10 ms round-trip time.

- **Supported runtimes versus any command.** amux's supported set is Claude Code, Codex CLI, and Gemini CLI, and its automation is built around what those runtimes do. Roost never spawns, supervises, or owns an agent: the CLI is an ordinary command inside a shell PTY, so any tool runs, including Claude Code. A separate list of ten CLIs also gets first-class status detection and a launcher entry; everything outside that list runs exactly as well, just unlabelled.

- **An orchestration product versus a terminal-fidelity product.** This is the real fork in the road. amux is trying to make many agents work unattended: watchdog, board, cron, cost accounting, agent-to-agent channels. Roost is trying to make one real terminal on any of your machines feel native from any device: panes and Arrange presets, keyboard parity, mobile key row and swipe deck, dictation into the session, Web Push on working / needs input / done. **Roost has no cost tracking and no task board today.** If unattended throughput is your problem, that gap matters.

- **Mobile.** Both take phones seriously; amux ships a PWA and a native iOS app for monitoring, approving, and recovering sessions. Roost's phone client is the same full application as the desktop one, with touch selection, a latching-Ctrl key row, a swipeable terminal-card deck, and a soft keyboard that offsets content instead of reflowing the terminal.

## Use amux instead if…

You want unattended overnight fleets of agents with auto-restart, cost accounting, and a kanban board on a single host. Roost deliberately owns none of that: it will not restart a crashed agent, compact its context, tell you what it spent, or hand it the next ticket, because it does not own the agent process at all. If your bottleneck is coordinating dozens of parallel runs rather than reaching a real terminal on the machine of your choice, amux is solving your problem and Roost is not.

## Links

- [amux.io](https://amux.io)
- [github.com/mixpeek/amux](https://github.com/mixpeek/amux)

See the whole field on the [alternatives hub](/alternatives/), or read what Roost does and does not do with agent processes in [agents](/docs/agents/).
