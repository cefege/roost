---
competitor: "cmux"
vendor: "Manaflow (YC S24)"
license: "GPL-3.0"
url: "https://cmux.com"
order: 1
category: "agent-terminal"
matrix:
  hostPlatforms: "macOS only"
  clientDevices: "the Mac it runs on, plus an iOS companion in TestFlight beta"
  multiMachine: "partial"
  zeroInstallClient: "no"
  persistentSessions: "yes"
  anyCli: "yes"
  mobileUx: "iOS companion in TestFlight beta, paired to one Mac"
  voiceInput: "no"
  pushAgentState: "Notification rings driven by OSC 9/99/777, surfaced on the Mac"
  selfHostedNoAccount: "yes"
verdict: "cmux is the better terminal on the one Mac it runs on; Roost is the only one of the two that reaches every machine you own from any browser."
pickRoostIf: "You have more than one machine and want a real terminal on any of them from a browser, including your phone."
useInsteadIf: "You work on one Mac at your desk and want the most polished native terminal, GPU rendering, and an in-app scriptable browser."
---

## Where they differ

- **One host versus a fleet.** cmux is an application that runs on your Mac and owns the terminals on that Mac. Roost is a coordinator plus any number of workers: every macOS, Linux, and Windows x64 machine you own on one coordinator, grouped in one sidebar with per-machine CPU, memory, disk, and network tiles. The coordinator machine is itself a worker, so a one-box install is the degenerate case rather than a different product.

- **What counts as the client.** cmux's client is the Mac it is installed on; reaching it from elsewhere means the TestFlight iOS companion or its beta SSH and remote-tmux paths, which are ways for a local app to reach a remote shell. Roost inverts the direction: the coordinator holds the sessions, any browser is a full client on a laptop, an iPhone, an Android phone, or an iPad, and workers dial outbound only so no machine exposes an inbound port.

- **Native rendering versus a cell-authoritative wire.** cmux gets fidelity by being a GPU terminal in one process. Roost has to earn the same fidelity over a network, so the worker owns the authoritative cell grid: rebuilt at one agreed width on resize so the browser never re-reflows history, every frame stamped with a monotonic `seq` so a stale viewer gets exactly one authoritative full frame, scrollback fetched on demand, double-width CJK and emoji as one atomic two-column span, mouse tracking forwarded only when the running program asked for it, and predictive local echo above roughly 10 ms of round-trip time, suppressed inside alt-screen TUIs.

- **A mobile companion versus the same app.** cmux's phone story is a companion paired to one Mac. Roost's phone client is the desktop application: touch selection, an on-screen key row with a latching Ctrl, a swipeable deck of terminal cards, a soft keyboard that offsets content instead of reflowing the terminal, and PWA install.

- **Agent state on the desk versus pushed to you.** cmux's notification rings fire on the Mac in front of you. Roost models working, needs input, and done on the sidebar row, the tab, the mobile card, and as a folder rollup, and delivers Web Push once a device grants permission, so a blocked agent finds you when you are not at the desk.

## What you give up either way

- **Choosing Roost costs you:** GPU-native rendering, the embedded scriptable WebKit browser, the per-tab git branch, pull request, and listening-port annotations, and the feel of a native AppKit app. Roost has no in-app browser and no per-agent cost accounting.
- **Choosing cmux costs you:** Linux and Windows hosts, any client that is not that Mac or its TestFlight companion, a phone-native UI, voice dictation, and push that reaches you away from the machine.

## Use cmux instead if…

You work on one Mac at your desk and want the most polished native terminal, GPU rendering, and an in-app scriptable browser. If your machine count is one and your client is always the keyboard in front of you, everything Roost spends on wire protocol, worker enrollment, and a browser client is overhead you will not get value from. The branch, pull request, and listening-port annotations on each tab are genuinely good triage affordances that Roost does not currently offer.

## What cmux is

cmux is a native macOS terminal for running coding agents, built by Manaflow (YC S24) in Swift and AppKit on top of `libghostty`, so the grid is GPU-rendered by the same engine behind Ghostty. It is macOS only, organised around a vertical tab list where each session carries the context you need to triage parallel runs: the git branch, the pull request, and the ports that session is listening on. It is free under GPL-3.0, with a paid Founders Edition for early access.

- **Also ships:** notification rings driven by the OSC 9/99/777 escapes agents already emit, an embedded scriptable WebKit browser, a CLI plus a Unix-socket API for driving sessions from outside, session restore that brings back scrollback across reboots, SSH workspaces and remote-tmux attach in beta, and an iOS companion in TestFlight paired to one Mac.

## Sources

- cmux (cmux.com)

Compare the rest of the field on the [alternatives hub](/alternatives/), or read how Roost's wire protocol keeps a browser honest in [terminal fidelity](/docs/terminal/).
