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
useInsteadIf: "You work on one Mac at your desk and want the most polished native terminal, GPU rendering, and an in-app scriptable browser."
---

## What cmux is

cmux is a native macOS terminal for running coding agents, built by Manaflow (YC S24) in Swift and AppKit on top of `libghostty`, so the grid is GPU-rendered by the same engine behind Ghostty. It is macOS only. Its organising idea is a vertical tab list where each session carries the context you actually need to triage parallel agent runs: the git branch, the pull request, and the ports that session is listening on.

It is a real agent host, not just a renderer. Notification rings are driven by the OSC 9/99/777 escapes agents already emit; an embedded scriptable WebKit browser lets you drive a page beside the terminal; a CLI plus a Unix-socket API make sessions scriptable from outside; and session restore brings sessions back with their scrollback across reboots. SSH workspaces and remote-tmux attach are in beta, and an iOS companion app is in TestFlight, paired to one Mac. cmux is free under GPL-3.0, with a paid Founders Edition for early access.

## Where they differ

- **A single host versus a fleet.** cmux is an application that runs on your Mac and owns the terminals on that Mac. Roost is a coordinator plus any number of workers: connect every macOS, Linux, and Windows x64 machine you own to one coordinator, and the sidebar groups every live session by machine with per-machine CPU, memory, disk, and network tiles. The coordinator machine is itself a worker, so a one-box install is the degenerate case rather than a different product.

- **What counts as the client.** cmux's client is the Mac it is installed on; reaching it from elsewhere means the TestFlight iOS companion or its beta SSH and remote-tmux paths, which are ways for a local app to reach a remote shell. Roost inverts the direction: the coordinator holds the sessions and any browser is a full client, on a laptop, an iPhone, an Android phone, or an iPad, with nothing to install. Workers dial outbound only and never expose an inbound port.

- **Native rendering versus a cell-authoritative wire.** cmux gets terminal fidelity by being a native GPU terminal in one process. Roost has to earn the same fidelity over a network, so the worker owns an authoritative cell grid: the grid is rebuilt at one agreed width on resize so the browser never re-reflows history, every frame carries a monotonic `seq`, a stale viewer gets exactly one authoritative full frame, scrollback is fetched on demand, and double-width CJK and emoji travel as one atomic two-column span. Mouse tracking is forwarded only when the running program asked for it, and predictive local echo engages above roughly 10 ms of round-trip time and is suppressed inside alt-screen TUIs.

- **Mobile as an afterthought versus mobile as the same app.** cmux's phone story is a companion paired to one Mac. Roost's phone client is the same application as the desktop one, with touch selection, an on-screen key row including a latching Ctrl, a swipeable deck of terminal cards, and a soft keyboard that offsets content instead of reflowing the terminal. It installs as a PWA.

- **Agent state on the machine versus agent state pushed to you.** cmux's rings fire on the Mac in front of you. Roost models working, needs input, and done on the sidebar row, the tab, the mobile card, and as a folder rollup, and delivers Web Push to a device once you grant it, so a blocked agent finds you when you are not at the desk.

- **Where each is deliberately smaller.** cmux ships things Roost does not: GPU-native rendering, an embedded scriptable browser, and branch/PR/port annotations per tab. Roost has no in-app browser and no per-agent cost accounting.

## Use cmux instead if…

You work on one Mac at your desk and want the most polished native terminal, GPU rendering, and an in-app scriptable browser. If your machine count is one and your client is always the keyboard in front of you, everything Roost spends on wire protocol, worker enrollment, and a browser client is overhead you will not get value from, and a native AppKit app will feel better than any browser can. The branch, PR, and listening-port annotations on each tab are also genuinely good triage affordances that Roost does not currently offer.

## Links

- [cmux.com](https://cmux.com)

Compare the rest of the field on the [alternatives hub](/alternatives/), or read how Roost's wire protocol keeps a browser honest in [terminal fidelity](/docs/terminal/).
