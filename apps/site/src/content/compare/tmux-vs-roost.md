---
competitor: "tmux"
vendor: "The tmux project"
license: "ISC"
url: "https://github.com/tmux/tmux"
order: 6
category: "classic-multiplexer"
matrix:
  hostPlatforms: "anywhere it compiles: macOS, Linux, and the BSDs"
  clientDevices: "any machine with an SSH client and a terminal emulator"
  multiMachine: "partial"
  zeroInstallClient: "no"
  persistentSessions: "yes"
  anyCli: "yes"
  mobileUx: "whatever your phone's SSH client gives you"
  voiceInput: "no"
  pushAgentState: "none; tmux has no notion of an agent"
  selfHostedNoAccount: "yes"
verdict: "tmux already gives you persistence on one host; Roost adds a zero-install client on any device, one view over every machine, and agent status."
pickRoostIf: "You want tmux-grade persistence plus a client on devices that have no SSH stack, across every machine you own."
useInsteadIf: "SSH plus a terminal emulator is all you need and you want zero moving parts."
---

## Where they differ

- **The client is the real difference.** tmux's client is a terminal emulator plus an SSH connection, which means every device you use has to have both. Roost's client is a browser tab, so a phone, a tablet, or a borrowed laptop is a full client with nothing installed — the same application, not a cut-down remote view. That is the entire reason Roost exists.

- **One server per machine versus one control plane over all of them.** With tmux, your mental model is a list of hosts you SSH into, because a tmux server is per-machine. Roost connects every macOS, Linux, and Windows x64 machine you own to one coordinator, and the sidebar groups every live session by machine with per-machine CPU, memory, disk, and network tiles. Workers dial outbound only, so no machine has to expose an inbound port, which is what makes a laptop behind NAT usable as part of the pool.

- **Agent state.** tmux has none, and structurally cannot: it will happily host a coding agent for a week, but it cannot tell you the agent in window 3 stopped and is waiting for an answer. Roost models working, needs input, and done, shows it on the sidebar row, the tab, the mobile card, and as a folder rollup, and can deliver Web Push to a device after you grant it. Ten CLIs get first-class detection and everything else still runs, just unlabelled.

- **Persistence is the part they share.** Roost is not trying to out-persist tmux. A keeper subprocess hosts every PTY and outlives worker restarts and updates, which gets you the same guarantee: drop WiFi, close the laptop, come back later, and the work and the scrollback are still there.

- **Ergonomics.** tmux is prefix keys and a config file, and that is a real advantage if your hands already know it. Roost is drag a tab to a pane edge to split, draggable dividers, Arrange presets (Grid, Columns, Rows, Main + stack, Equalize), ⌘/Ctrl+1–9 tab switching, and ⌘Enter spotlight, with a separate Windows binding set so plain Ctrl+letter still reaches the PTY. Neither is more correct; one is muscle memory, the other is discoverable.

- **Roost does not replace tmux.** They compose. A Roost session is a real PTY, so `tmux` inside it works exactly as it always has: tmux for in-session pane muscle memory, Roost for reaching the machine at all. Nothing about Roost asks you to give up your config.

## What you give up either way

- **Choosing Roost costs you:** zero moving parts and decades of hardening. tmux is on every box already, with no server to enroll, no browser, and no keys to pair; Roost adds a coordinator, enrolled workers, and a paired device.
- **Choosing tmux costs you:** any client without an SSH stack and a terminal emulator, one view over many machines with per-machine metrics, any notion of what is running inside a pane, and notifications of any kind.

## Use tmux instead if…

SSH plus a terminal emulator is all you need and you want zero moving parts. If you only ever work from one desk with one terminal you already trust, adding a coordinator and workers buys you nothing you will feel. Come back to Roost when the thing you actually want is a real terminal on a machine you are not sitting at, from a device that has no SSH client.

## What tmux is

tmux is the baseline every tool on this page is measured against, and it has earned that. It is a terminal multiplexer: sessions that keep running after you detach, windows and splits inside them, and reattach from anywhere you can SSH. It is ISC licensed, packaged everywhere, configured with a text file, and driven by a prefix key.

- **Zellij is the same category** with different ergonomics — a Rust multiplexer with discoverable keybindings, floating panes, and layouts — and everything on this page applies to it too.

## Sources

- tmux source (github.com/tmux/tmux)
- Zellij (zellij.dev)

See the whole field on the [alternatives hub](/alternatives/), or read how Roost keeps sessions alive in [fleet](/docs/fleet/).
