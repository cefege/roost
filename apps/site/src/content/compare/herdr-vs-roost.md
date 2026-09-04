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
verdict: "herdr is the better answer when your client is always a terminal; Roost trades in-process fidelity for reaching any enrolled macOS or Linux machine from any browser."
pickRoostIf: "You want the same terminal from a phone, a tablet, or a borrowed laptop, on macOS and Linux machines that never expose an inbound port."
useInsteadIf: "You live in one terminal emulator over SSH and want agent-state awareness without any browser or server."
---

## Where they differ

- **The client is a terminal versus a browser.** herdr renders a TUI inside the terminal emulator you already use, which is exactly why it feels fast and native. Roost's client is a browser tab, which is why it can be a phone: an iPhone, an Android phone, or an iPad is a full client with nothing to install. Neither is strictly better — herdr pays nothing for rendering and cannot be opened on a device without an SSH client.

- **One host per attach versus one control plane over many.** herdr runs where the work is; reaching a second machine means SSHing to it and attaching that machine's herdr. Roost connects your macOS and Linux machines to one coordinator and shows all of them in a single sidebar grouped by machine, with per-machine CPU, memory, disk, and network tiles. Workers dial outbound only, so no host exposes an inbound port, and `roost add-machine --platform macos|linux` prints a one-shot 24-hour enrollment command.

- **Fidelity for free versus fidelity rebuilt over a network.** herdr is in the same process as your terminal, so fidelity costs nothing. Roost has to make a browser behave: an authoritative cell grid rebuilt at one agreed width on resize, a monotonic `seq` on every frame so a reconnecting viewer gets one authoritative full frame rather than duplicated history, on-demand scrollback, atomic two-column spans for double-width CJK and emoji, mouse tracking forwarded only when the program requested it, and predictive local echo above roughly 10 ms.

- **Notification reach.** herdr tells you an agent is blocked on the screen you are looking at. Roost carries the same three states — working, needs input, done — to the sidebar row, the tab, the mobile card, and a folder rollup, and can deliver Web Push to a phone once that device grants permission. Nothing about the status is persisted; a restart re-derives it.

- **Roost's detection is downstream of herdr's.** Roost's first-party OMP and Pi agent-lifecycle integrations, and its screen and OSC-title detection manifests, were adapted from herdr at commit `eacea2da` under Apache-2.0. The attribution sits in the source headers of `apps/worker/src/agent-status/`: herdr solved agent-state detection well, and Roost's detection is not an independent invention.

## What you give up either way

- **Choosing Roost costs you:** one dependency-free binary with no server to think about, a plugin ecosystem, an agent-facing socket API designed so agents coordinate with each other, and support for more agents out of the box than Roost's ten first-class detections. Roost has no plugin system.
- **Choosing herdr costs you:** any client that is not a terminal emulator, a phone or tablet surface beyond whatever SSH app you run, one view over many machines with per-machine metrics, voice dictation, and notifications that reach you away from the screen.

## Use herdr instead if…

You live in one terminal emulator over SSH and want agent-state awareness without any browser or server. If your workflow is already `ssh box && attach`, herdr adds the one thing that workflow is missing — knowing which agent is blocked — while changing nothing else, and a single Rust binary is far less to operate than a coordinator with enrolled workers. Choose Roost only when you genuinely need the fleet view or a client that is not a terminal.

## What herdr is

herdr is an agent multiplexer that lives in your terminal: a single Apache-2.0 Rust binary, no Electron and no heavy dependencies, running on macOS and Linux with Windows in beta. It is a background server with the terminals inside it, so agents keep working when you close the lid or drop the network, and you reattach from any terminal or over SSH. Its distinguishing feature is agent-state awareness: every pane is marked working, blocked, or idle by inspecting what the agent actually prints, so you never hunt for the one that stopped and needs an answer.

- **Also ships:** tmux-style prefix keys and click-drag-split mouse interaction, both first-class; workspaces organised around git repos or folders with their own tabs and panes; sessions that survive restarts; and a CLI plus socket API that agents themselves can drive to spawn panes and wait on each other.

## Sources

- herdr (herdr.dev)
- herdr source (github.com/herdrdev/herdr)

See the whole field on the [alternatives hub](/alternatives/), or read how Roost derives agent status in [agents](/docs/agents/).
