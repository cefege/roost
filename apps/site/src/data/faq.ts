// The landing page FAQ. Rendered as a definition list *and* serialised into the
// FAQPage JSON-LD block on the same page, so answers must stay plain text: no
// markdown, no HTML, no backticks — whatever is written here is what a search
// engine reads out loud.
//
// Every answer is checked against src/content/docs/*.md; keep the two in sync.

export const FAQ: { q: string; a: string }[] = [
  {
    q: "Does Roost replace my terminal?",
    a: "No. Roost replaces SSH-ing around to find a session, not the terminal itself. Every Roost session is a real shell PTY with full ANSI colour, scrollback, mouse modes and hyperlinks, rendered in a browser instead of a terminal emulator. If tmux or Zellij is how you like to work, run it inside a Roost session.",
  },
  {
    q: "Does it work with Claude Code?",
    a: "Yes. A Roost session is an ordinary shell PTY, so Claude Code runs in it exactly as it does in your own terminal, with its real interface rather than a chat wrapper. Roost never spawns, supervises or owns an agent process. Ten CLIs (Codex, Gemini CLI, OpenCode, Cursor Agent, Amp, GitHub Copilot CLI, Droid, Grok, Pi and OMP) additionally get a working / needs input / done badge; everything else runs fine, just unlabelled.",
  },
  {
    q: "Do I need Tailscale?",
    a: "For the supported setup, yes. Tailscale Serve publishing the coordinator's loopback listener on port 4102 is the only topology the installer configures, the only one roost quickstart produces, and the only one the release canaries exercise. Tailscale is also the private transport between browsers, coordinator and workers, the trusted enrollment boundary, and the source of real TLS certificates, which is how a phone connects with no port forwarding.",
  },
  {
    q: "Does it work without Tailscale?",
    a: "Partly, and only in ways the docs are explicit about. The optional Cloudflare Access plus Tunnel path lets a browser-only device reach your fleet with nothing but a browser, while the coordinator and every worker still talk over Tailscale. WireGuard, Headscale, ZeroTier and a plain LAN can be wired up by hand once browser-to-coordinator and worker-to-coordinator reachability is solved, but the installer does not configure them and the canaries do not exercise them, so you own the transport, the certificates and the enrollment boundary.",
  },
  {
    q: "Is my code uploaded anywhere?",
    a: "No. The coordinator, the workers and every PTY run on hardware you own, over your own network. There is no analytics, no crash reporting, no phone-home and no vendor account in the loop; diagnostics are local log files that roost doctor reads from disk. The only things that leave your machines are ones you deliberately configure, such as a Deepgram key for dictation or a Cloudflare tunnel you set up yourself.",
  },
  {
    q: "Does it run on Windows?",
    a: "Yes. Windows x64 machines run coordinators and workers as restricted SCM services under a dedicated low-privilege roost-operator identity that is denied interactive logon, installed by a signed PowerShell bootstrap that pins the publisher certificate. Windows also gets its own keyboard bindings — Alt and Alt+Shift chords instead of Command chords — so a plain Ctrl plus letter still reaches the program running in the PTY.",
  },
  {
    q: "Can I use it from a phone?",
    a: "Yes, and it is the same app rather than a cut-down companion. Add Roost to the home screen and you get a standalone PWA with touch text selection, an on-screen key row with a latching Ctrl, a swipeable deck of terminal cards, and a soft keyboard that shifts the layout instead of reflowing the terminal. On iPhone and iPad, installing to the home screen is also the prerequisite for OS notifications.",
  },
  {
    q: "What happens when my laptop sleeps?",
    a: "The work keeps running. PTYs live in a keeper subprocess on the worker machine that outlives worker restarts and updates, so closing the lid on the device you were browsing from does not touch the session. When you come back, on that device or another one, the browser reconnects, sends the last sequence number it applied, and receives exactly what it missed — nothing duplicated, nothing dropped, scrollback intact.",
  },
  {
    q: "Can a team share one Roost?",
    a: "No. Roost is single-user today: every device you pair is one of your own devices, and there are no accounts, roles, shared tokens or per-user permissions. Access control is per device — pair a browser to grant it, delete its row to revoke it immediately.",
  },
  {
    q: "Is it free?",
    a: "Yes. Roost is free and open source under GPL-3.0-only, with no paid tier, no hosted plan and no account to create. You self-host it, so the only thing it costs is the hardware you already own.",
  },
  {
    q: "How do I add a machine?",
    a: "On the coordinator, run roost add-machine with --platform macos, linux or windows, or use Settings, Machines, Add machine. Each invocation mints a one-shot bootstrap token that expires 24 hours after minting and prints the enrollment command for that platform. Paste that command on the new host: machines join by pulling, so the coordinator never SSHes out, and the worker appears in Settings, Machines within a few seconds.",
  },
  {
    q: "How do I update the fleet?",
    a: "Run roost push from a clean checkout at the commit you pushed. It upgrades and proves the coordinator before touching any remote worker, deploys that exact commit to macOS and Linux hosts, and sends authenticated Windows workers through the signed updater service. Every host activates through a journal with a health proof and automatic rollback, and push waits for a fresh post-update heartbeat from each target before it reports success. A single machine can also self-update with roost update.",
  },
];
