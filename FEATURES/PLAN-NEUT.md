# PLAN-NEUT — Neutralino + cross-platform rollout

Status: DEFERRED feature plan. Not implementing now. See chat 2026-06-14.

Anchor: `PLAN-NEUT`. Stages: `S0`..`S4`. Substeps: `S<N>.<n>`.

## Personas (drives every stage)
- **P1 local-only** — single Mac, never connects another. Sees app, not coord/worker split.
- **P2 expanding** — starts local, later adds Macs via pair flow.
- **P3 server/headless** — installs coord-only on a server; workers optional.

## Product shape (one codebase, three deploy modes)
| Mode | Runs | Audience |
|---|---|---|
| Neutralino app | coord+worker LaunchAgents + Neutralino window | P1, P2 (primary product) |
| Browser | coord+worker LaunchAgents, browser tab on :4102 | dev/debug surface, P3 |
| Worker-only | worker LaunchAgent, pairs to remote coord | P2 additional Mac |

Neutralino is a thin window over the existing SPA. Coord/worker run independently of Neutralino window state.

## Stage 0 — prerequisites
- **S0.1 unified install CLI** — collapse `apps/coord/scripts/install.sh` + `apps/worker/scripts/install.sh` into `apps/roost-cli/src/main.ts install [--coord-only|--worker-only|--browser|--add-app]`.
- **S0.2 pair flow UI** — surface in `apps/web/src/components/Settings/WorkersPane.tsx`. Backend exists at `apps/coord/src/router/pair.ts`.
- **S0.3 no-workers empty state** — sidebar must render cleanly for P3.

## Stage 1 — Neutralino macOS
- **S1.1** new `apps/desktop/` (Neutralino project). `neutralino.config.json` → `url: http://localhost:4102`, window size, tray icon.
- **S1.2** sidecar mode OFF. coord+worker stay LaunchAgents. Neutralino crash ≠ coord crash.
- **S1.3** keybindings in new `apps/web/src/keybindings.ts`. `Cmd+T` new pane, `Cmd+W` close pane, `Cmd+K` clear, `Cmd+1..9` switch pane. Guard `if (window.Neutralino)` so browser mode still works.
- **S1.4** WebKit compat audit — grep `apps/web/src/` for Chrome-only APIs (`showOpenFilePicker`, `chrome.*`). wterm is WASM so likely clean.
- **S1.5** Apple Developer signing + notarization. Output `.dmg`. `roost install` drops `.app` in `/Applications`.
- **S1.6** verification: humanchrome on browser mode remains the e2e path; Neutralino smoke is manual launch + tray check.

## Stage 2 — Windows port
- **S2.1** sysinfo: replace `vm_stat`/`sysctl`/`df` in `apps/worker/src/heartbeat.ts` with `systeminformation` npm pkg. No platform branching.
- **S2.2** config paths: new `apps/shared/src/paths.ts`. mac=`~/Library/Application Support/Roost/`, win=`%APPDATA%\Roost\`, linux=`~/.config/roost/`. Replace every hardcoded mac path.
- **S2.3** node-pty Windows: ConPTY, default shell PowerShell. Strip `codesign`/`xattr` repair steps on non-mac in `apps/roost-cli/src/main.ts deploy`.
- **S2.4** installer: `node-windows` Service or Task Scheduler. `roost install` detects platform, picks supervisor.
- **S2.5** Neutralino Windows build: same `apps/desktop/` source, `neu build --target win`. WebView2 (Chromium) → fewer compat surprises than mac.
- **S2.6** Windows signing: EV cert OR accept SmartScreen warning early on.

## Stage 3 — Linux port
- **S3.1** sysinfo: already cross-platform via S2.1.
- **S3.2** config paths: already done via S2.2.
- **S3.3** node-pty Linux: native PTY, default `$SHELL`. Cleanest of the three.
- **S3.4** installer: systemd user unit at `~/.config/systemd/user/roost.service`.
- **S3.5** Neutralino Linux: WebKitGTK — S1.4 compat audit covers it.
- **S3.6** distribution: `.AppImage` first; `.deb`/`.rpm` later.

## Stage 4 — distribution polish
- **S4.1** Neutralino built-in auto-update wired up.
- **S4.2** Homebrew formula (mac), winget manifest (win), AUR/Flatpak (linux).
- **S4.3** `roost --version` + version field in heartbeat → coord UI flags mixed-version fleets.

## Gates (stage ordering, blocking)
- S0 → S1: install CLI must work; Neutralino assumes it.
- S1 → S2: Mac Neutralino must ship + have real users before splitting effort cross-platform. If nobody wants the desktop app, skip Windows.
- S2 → S3: Windows is the hard port; if it lands, Linux is mechanical.

## Effort estimate
- S0: 3-5 days
- S1: ~1 week incl signing
- S2: 1-2 weeks
- S3: 3-5 days
- S4: ongoing

## Out of scope
- Mobile (iOS/Android) — webview works but worker can't run there.
- ARM Windows / Linux — not S1-S4 concern.
- Bonjour/mDNS auto-pair — orthogonal.
- Plugin system — orthogonal.

## Open questions (resolve at S0 kickoff)
- Single LaunchAgent supervisor vs. keep `com.roost.coordinator-v2` + `com.roost.worker-v2` split? Lower risk = keep split; better UX = supervisor.
- `.pkg` vs. `curl|bash` for initial mac distribution? curl|bash first, `.pkg` at S4.
- Neutralino tray icon menu items: `Open`, `Status`, `Logs`, `Quit` — confirm with P1 walkthrough.
