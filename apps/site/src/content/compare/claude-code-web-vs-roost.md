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
useInsteadIf: "You want zero infrastructure and are happy for Claude Code alone to run on someone else's VM."
---

## What Claude Code on the web is

Claude Code on the web runs Claude Code sessions in isolated Anthropic-managed VMs, driven from the browser or the Claude mobile apps. The sandbox is deliberately locked down: network egress goes through an allowlist proxy, and the GitHub token lives in a separate proxy outside the sandbox so the agent never holds the credential directly. Enterprises can route sessions to self-hosted infrastructure instead of Anthropic's. Separately, `claude rc` tethers one local machine so a web or mobile session can drive it.

It is the same Claude Code you would run in a terminal, with the terminal removed: you get a task-shaped view of what the agent did, not a shell. There is nothing to install and nothing to operate, which is the entire point.

## Where they differ

Point by point:

| | Claude Code on the web | Roost |
|---|---|---|
| **What it drives** | Anthropic-managed cloud VMs, or one local machine tethered with `claude rc` | Every macOS, Linux, and Windows x64 machine you own, natively |
| **Open a new terminal while away** | No; you are limited to sandboxes or sessions already running | Yes; open a fresh terminal in any folder on any machine, from your phone |
| **What runs in it** | Claude Code | Any agent, shell, REPL, or TUI — including Claude Code |
| **Where your code lives** | A cloud VM, or the one tethered machine | Your own hardware, your own network |
| **The surface** | A task view onto the agent | The real terminal, with full ANSI, scrollback, mouse, and touch |
| **Hosting** | SaaS, tied to an account | Self-hosted, no account, no telemetry |
| **Who holds credentials** | An egress-allowlist proxy and a separate GitHub-token proxy, outside the sandbox | Whatever is already on your machine; Roost adds no credential broker |
| **Client keys** | Your Anthropic account session | A per-device Ed25519 key minted in the browser, non-extractable, revocable by deleting a row |

Beyond the table, five things are worth stating plainly:

- **The direction of control is inverted.** Anthropic's product is a control plane *they* own, talking to machines *they* manage. Roost is a control plane *you* own, talking to as many of *your* machines as you like, with no third party in the loop. That is a difference in who is trusted, not in features.

- **"New terminal on demand" is the sharp edge.** Away from your desk, the web product can only continue what already exists in a sandbox or on the tethered machine. Roost lets you browse folders on any enrolled worker from a phone and choose *Open terminal here*, on a machine you are nowhere near.

- **Session lifetime is bounded by different things.** A managed sandbox lives as long as the session Anthropic is running for you, and the `claude rc` tether lasts as long as that one machine stays awake and connected. In Roost a keeper subprocess hosts every PTY and outlives worker restarts and updates, so closing the lid, losing WiFi, or updating the fleet with `roost push` does not end the session — reattach later from a different device and the full scrollback is still there.

- **Integration surface, honestly.** Claude Code is reachable from a terminal, the web, iOS, Android, VS Code, JetBrains, GitHub Actions, and Slack, and Anthropic maintains every one of those paths. Roost has exactly one client, the browser, and integrates with nothing else: no editor plugin, no CI action, no chat bot.

- **Roost is not competing on convenience.** There is real infrastructure to run: a coordinator, enrolled workers, and a tailnet. Anthropic's version has none of that, and for a lot of work that is the right trade.

Your browser stays perfectly usable for claude.ai either way. Roost is not a replacement for it; it is the piece a managed cloud cannot be, which is native control of your own fleet from anywhere.

## Use Claude Code on the web instead if…

You want zero infrastructure and are happy for Claude Code alone to run on someone else's VM. If your work fits in a sandbox with an allowlisted network, and you would rather have Anthropic operate the isolation, the credential proxying, and the machine than run a coordinator yourself, then this is strictly less to maintain and the security posture is well-considered. Roost only wins when the code has to stay on your hardware, or the tool you need is not Claude Code, or you need a real shell.

## Links

- [Claude Code](https://claude.com/product/claude-code)
- [Claude Code on the web](https://claude.ai/code)

See the whole field on the [alternatives hub](/alternatives/), or read how device keys work in [security](/docs/security/).
