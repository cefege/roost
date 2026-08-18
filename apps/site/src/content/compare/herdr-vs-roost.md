---
competitor: "herdr"
vendor: "herdrdev"
license: "Apache-2.0"
url: "https://herdr.dev"
order: 3
category: "agent-terminal"
matrix:
  hostPlatforms: "macOS and Linux, with Windows in beta"
  clientDevices: "any terminal emulator, locally or over SSH"
  multiMachine: "partial"
  zeroInstallClient: "no"
  persistentSessions: "yes"
  anyCli: "yes"
  mobileUx: "none beyond whatever SSH client you run on the phone"
  voiceInput: "no"
  pushAgentState: "working / blocked / idle marked on every pane in the terminal UI"
  selfHostedNoAccount: "yes"
useInsteadIf: "You live in one terminal emulator over SSH and want agent-state awareness without any browser or server."
---

## What herdr is

herdr is an agent multiplexer that lives in your terminal: a single Rust binary, no Electron and no heavy dependencies, running on macOS and Linux with Windows in beta. It is a background server with the terminals inside it, so agents keep working when you close the lid or drop the network, and you reattach from any terminal or over SSH. Sessions survive restarts.

Its distinguishing feature is agent-state awareness. Every pane is marked working, blocked, or idle by inspecting what the agent actually prints, so you never hunt for the one that stopped and needs an answer. It keeps tmux-style prefix keys and click-drag-split mouse interaction both first-class, organises work into workspaces around git repos or folders with their own tabs and panes, and exposes a CLI and socket API that agents themselves can drive to spawn panes and wait on each other. It is Apache-2.0 licensed.

**Disclosure.** Roost's first-party OMP and Pi agent-lifecycle integrations, and its screen and OSC-title detection manifests, were adapted from herdr at commit `eacea2da` under Apache-2.0. The attribution sits in the source headers of `apps/worker/src/agent-status/`. herdr solved agent-state detection well, and Roost's detection is downstream of that work rather than an independent invention.

## Where they differ

- **The client is a terminal versus the client is a browser.** herdr renders a TUI inside the terminal emulator you already use, which is exactly why it feels fast and native. Roost's client is a browser tab, which is why it can be a phone. Neither is a strictly better answer: herdr pays nothing for rendering and cannot be opened on a device without an SSH client; Roost pays for a wire protocol and gets an iPhone, an Android phone, and an iPad as full clients with nothing to install.

- **One host per attach versus one control plane over many.** herdr runs where the work is; reaching a second machine means SSHing to it and attaching that machine's herdr. Roost connects every macOS, Linux, and Windows x64 machine to one coordinator and shows all of them in a single sidebar grouped by machine, with per-machine CPU, memory, disk, and network tiles. Workers dial outbound only, so no host exposes an inbound port, and `roost add-machine --platform …` prints a one-shot 24-hour enrollment command.

- **Terminal-native fidelity versus fidelity reconstructed over a network.** herdr is in the same process as your terminal, so fidelity is free. Roost has to make a browser behave: the worker owns an authoritative cell grid rebuilt at one agreed width on resize, every frame carries a monotonic `seq` so a reconnecting viewer gets one authoritative full frame rather than duplicated history, scrollback is fetched on demand, double-width CJK and emoji are atomic two-column spans, mouse tracking is forwarded only when the program requested it, and predictive local echo covers latency above roughly 10 ms.

- **Notification reach.** herdr tells you an agent is blocked on the screen you are looking at. Roost carries the same three states — working, needs input, done — to the sidebar row, the tab, the mobile card, and a folder rollup, and can deliver Web Push to a phone once that device grants permission. Nothing about the status is persisted; a restart re-derives it.

- **Where herdr is ahead.** It is one binary with no server to think about, it has a plugin ecosystem and an agent-facing socket API designed for agents to coordinate with each other, and it supports more agents out of the box than Roost's ten first-class detections. Roost has no plugin system.

## Use herdr instead if…

You live in one terminal emulator over SSH and want agent-state awareness without any browser or server. If your workflow is already `ssh box && attach`, herdr adds the one thing that workflow is missing — knowing which agent is blocked — while changing nothing else, and a single Rust binary is far less to operate than a coordinator with enrolled workers. Choose Roost only when you genuinely need the fleet view or a client that is not a terminal.

## Links

- [herdr.dev](https://herdr.dev)
- [github.com/herdrdev/herdr](https://github.com/herdrdev/herdr)

See the whole field on the [alternatives hub](/alternatives/), or read how Roost derives agent status in [agents](/docs/agents/).
