---
competitor: "Claude Code on the web"
vendor: "Anthropic"
license: "Proprietary"
url: "https://claude.com/product/claude-code"
order: 8
category: "cloud-agent"
matrix:
  hostPlatforms: "Anthropic-managed cloud VMs; enterprises may route sessions to self-hosted infrastructure; `claude rc` tethers one local machine"
  clientDevices: "any browser, plus the Claude iOS and Android apps"
  multiMachine: "no"
  zeroInstallClient: "yes"
  persistentSessions: "partial"
  anyCli: "no"
  mobileUx: "the Claude mobile apps drive sessions that are already running"
  voiceInput: "no"
  pushAgentState: "Session progress in the Claude web and mobile UI"
  selfHostedNoAccount: "no"
verdict: "Claude Code on the web runs one agent on machines Anthropic manages; Roost runs any CLI in a real terminal on macOS and Linux machines you own."
pickRoostIf: "You want your code to stay on your own macOS or Linux hardware and any CLI, not just Claude Code, to run in a terminal you can open from a phone."
useInsteadIf: "You want zero infrastructure and are happy for Claude Code alone to run on someone else's VM."
---

## Where they differ

- **Who owns the machines.** Anthropic runs the control plane and the VMs — or tethers one local machine with `claude rc`. Roost's released, self-hosted product keeps terminal workers on your macOS and Linux machines and lets you operate the control plane yourself. A per-account managed implementation is qualified but not launched; production signup and the shared dashboard origin are inactive, and accounts can only be operator-created.

- **A task view versus the real terminal.** The web product gives you a task-shaped view of what Claude Code did, and what runs in it is Claude Code. Roost gives you the real terminal, with full ANSI, scrollback, mouse, and touch, and what runs in it is any agent, shell, REPL, or TUI — including Claude Code, which Roost treats as an ordinary command.

- **"New terminal on demand" is the sharp edge.** Away from your desk, the web product can only continue what already exists in a sandbox or on the tethered machine. Roost lets you browse folders on any enrolled worker from a phone and choose *Open terminal here*, on a machine you are nowhere near.

- **Session lifetime is bounded by different things.** A managed sandbox lives as long as the session Anthropic is running for you, and the `claude rc` tether lasts as long as that one machine stays awake and connected. In Roost a keeper subprocess hosts every PTY and outlives worker restarts and updates, so closing the lid, losing WiFi, or updating the fleet with `roost push` does not end the session — reattach later from a different device and the full scrollback is still there.

- **Credentials and client identity.** The sandbox is deliberately locked down: network egress goes through an allowlist proxy, and the GitHub token lives in a separate proxy outside the sandbox so the agent never holds the credential; your client identity is your Anthropic account session. Roost adds no credential broker — it uses whatever is already on the machine — and each device holds a per-device Ed25519 key minted in the browser, non-extractable, revocable by deleting a row.

- **Where the code lives.** Claude Code runs in a cloud VM or on one tethered machine. Roost's released product keeps the shell and code on workers you connect: it is self-hosted, accountless, and has no telemetry. Enterprises can route Claude web sessions to self-hosted infrastructure instead of Anthropic's.

## What you give up either way

- **Choosing Roost costs you:** managed execution sandboxes and most integration surfaces. You still provide and enroll the machines where terminals run; you also operate a coordinator and its browser-trusted HTTPS endpoint, using automatic Tailscale Serve or direct HTTPS. Roost has one client, the browser, with no editor plugin, CI action, or chat bot, while Anthropic maintains terminal, web, iOS, Android, VS Code, JetBrains, GitHub Actions, and Slack paths.
- **Choosing Claude Code on the web costs you:** a shell, panes, and scrollback; more than one machine; opening a fresh terminal while away; any CLI other than Claude Code; and code that never leaves hardware you control.

## Use Claude Code on the web instead if…

You want managed cloud execution and are happy for Claude Code alone to run on someone else's VM. If your work fits in a sandbox with an allowlisted network, Anthropic operates the isolation, credential proxying, and machine. Roost is not a replacement for that: its released product is self-hosted and supplies neither execution sandboxes nor agent machines, and it wins only when the code must stay on your hardware, the tool is not Claude Code, or the work needs a real terminal.

## What Claude Code on the web is

Claude Code on the web runs Claude Code sessions in isolated Anthropic-managed VMs, driven from the browser or the Claude mobile apps. It is the same Claude Code you would run in a terminal, with the terminal removed: you get a task-shaped view of what the agent did, not a shell. There is nothing to install and nothing to operate, which is the entire point.

## Sources

- Claude Code (claude.com/product/claude-code)
- Claude Code on the web (claude.ai/code)

See the whole field on the [alternatives hub](/alternatives/), or read how device keys work in [security](/docs/security/).
