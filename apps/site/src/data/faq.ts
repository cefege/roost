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
    q: "How do I access managed Roost?",
    a: "Managed Roost is a private operator-provisioned service. The initial owner uses https://dashboard.roosttt.com/login to authorize a browser and https://dashboard.roosttt.com/app to open the one managed dashboard. The open-source edition remains available separately for anyone who wants to operate Roost themselves.",
  },
  {
    q: "Does it work with Claude Code?",
    a: "Yes. A Roost session is an ordinary shell PTY, so Claude Code runs in it exactly as it does in your own terminal, with its real interface rather than a chat wrapper. Roost never spawns, supervises or owns an agent process. Ten CLIs (Codex, Gemini CLI, OpenCode, Cursor Agent, Amp, GitHub Copilot CLI, Droid, Grok, Pi and OMP) additionally get a working / needs input / done badge; everything else runs fine, just unlabelled.",
  },
  {
    q: "Do I need Tailscale to self-host?",
    a: "No. With no endpoint flags, roost quickstart uses Tailscale Serve as the automatic convenience topology and obtains HTTPS for the tailnet hostname. It can instead serve a browser-trusted certificate directly when you provide the grouped HTTPS coordinator URL, absolute certificate path and absolute key path. Tailscale supplies reachability only; every browser and worker still needs its own scoped one-shot grant or approved pairing.",
  },
  {
    q: "What does Tailscale provide to self-hosted Roost?",
    a: "Automatic mode uses Tailscale for private coordinator reachability and convenient browser-trusted HTTPS without port forwarding. Explicit-certificate mode leaves reachability, DNS and certificate issuance to you. A tailnet address is never an enrollment credential, and other private overlays likewise do not replace Roost grants or pairing.",
  },
  {
    q: "Is my code uploaded to a managed sandbox?",
    a: "No. Managed Roost hosts the owner account and dashboard, but each shell PTY runs on a worker machine you connect; Roost is not a vendor execution sandbox. With the open-source edition, the coordinator, workers and dashboard are all self-hosted. Optional integrations can still send the data they are configured for, such as dictation audio sent to Deepgram.",
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
    q: "Can a team share managed Roost?",
    a: "Not in the initial managed launch. It has one operator-provisioned owner and one dashboard. The self-hosted edition remains accountless and user-operated: pair a browser to grant it access and delete its device row to revoke it.",
  },
  {
    q: "Is the self-hosted edition free?",
    a: "Yes. The self-hosted edition is free and open source under GPL-3.0-only. It is accountless and runs the coordinator, workers, and dashboard on infrastructure you operate.",
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
