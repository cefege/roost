# Track U — U-2 and U-3 wave plan (working artifact, not a repo doc)

Measured v2 (`/home/almalinux/repos/roost/apps/web/src`): 453 `.ts`/`.tsx`, 65,282 lines.
U-1 already ported the client/store half (~28k lines of Rust). What remains is the
transport half (`client/carriers`, `client/attachments`, `client/input`, `store/transport`),
the renderer satellites, and the whole Solid UI.

## U-2 — eight slices, one batch, after the U-1c lib is green

| Slice | v2 sources | Lands in |
|---|---|---|
| **A1** WebRTC carrier | `client/carriers/*` (10 files, 1339), `store/transport/terminal-peer*.ts` (5 files, 1209), `client/carriers/terminal-direct-browser.ts` | logic → `roost-client-core/src/client/carriers/`; `RtcPeerConnection` → `roost-web/src/platform/rtc_peer.rs` |
| **A2** loopback / local-first + outbound sync | `client/carriers/attachment-loopback.ts`, `localBootstrap.ts`, `localWorkerDiscovery.ts`, `store/transport/local-terminal*.ts` (2 files, 760), `store/transport/sync-outbound.ts` (333), `client/sync/sync-flow.ts` | `roost-client-core/src/client/local/`, `roost-web/src/platform/local_terminal.rs`, `sync_outbound.rs` |
| **A5** attachments | `client/attachments/*` (7 files, 1069) | `roost-client-core/src/client/attachments/` |
| **A7** predictive echo / input | `client/input/predictiveEcho*.ts` (2), `predictiveEchoGrid.ts` (97), `predictiveEchoExpiry.ts` (65), `terminalInputHistory.ts` (51), `terminalInputStatus.ts` (39), `renderer/predictiveEcho.ts` (394) | `roost-client-core/src/client/input/echo*` + `roost-web-terminal/src/predictive_echo*` |
| **R2** scheduler / backfill / preview / snapshot | `renderer/terminal-render-scheduler.ts` (339), `renderer/scrollbackBackfill.ts` (389), `renderer/terminalPreview.ts` (134), `renderer/terminalSnapshotFacade.ts` (121), `client/terminal-stream/{cellHistoryRanges,scrollbackDemandBounds,scrollbackBackfillState}.ts` (320) | `roost-web-terminal/src/scheduler*`, `backfill*`, `preview*`, `snapshot_facade*` |
| **R3** input / IME controller | `renderer/terminalInputController.ts` (227), `terminalComposeSelection.ts` (260), `terminalSelectionGuard.ts` (361), `client/input/terminalInput.ts` (181) | `roost-web-terminal/src/input_controller*`, `selection_guard*` |
| **R6** incident capture | `renderer/terminalIncidentDom.ts` (397), `terminalIncidentCaptureState.ts` (395), `terminalIncidentCapture.ts` (368), `terminalIncidentCaptureEvidence.ts` (284), `terminalIncidentCaptureObserver.ts` (261), `terminalIncidentCaptureRpc.ts` (158) | `roost-web-terminal/src/incident*` |
| **S1** app shell + routes + md primitives | `apps/web/src/styles/theme-vars.css`, `components/Settings/md/tokens.css`, `components/Settings/md/primitives.tsx`, `components/design/DesignGallery.tsx` | `roost-web/src/routes/*`, `roost-web/src/components/md/*`, `roost-web/src/app_shell.rs` |

## U-3 — seven slices, one batch, after U-2

| Slice | v2 source | Lines |
|---|---|---|
| terminal glue + startup overlay | `components/terminal/*` (26 files) | 4737 |
| Settings panes | `components/Settings/*` (22 files) | 2972 |
| pairing / onboarding | `components/pairing/*` (16 files) | 1880 |
| browse / machines / agents / search | `components/{browse,machines,agents,search,sidebar}/*` (42 files) | 5025 |
| deck / layout / workbench | `components/{deck,layout}/*` (24 files) | 3858 |
| notifications / palette / toasts | `components/{notifications,palette}/*` (18 files) | 1875 |
| voice + static assets + **A10 `window.__smoke`** | `voice/*` (9 files), `smoke/*` (10 files), `public/*` | 2134 + assets |

## The bar for Track U done (from the assignment, not negotiable)

1. two agreeing green `cargo test -p roost-client-core -p roost-web-terminal --no-fail-fast`
2. `dx build --release -p roost-web --platform web --features smoke` drivable, and
   `grep -c __smoke` = 0 over a production build without the feature
3. every route renders; every `data-testid` the specs use is in the bundle
4. commit, push, `write agent://Main`

## The DOM/testid contract — never break

`div.wterm.cell-grid`, `.cell-scrollback`, `.cell-viewport`, `div.cell-row`
(`className` EXACTLY `cell-row`), `.cell-find-hit` (+ `cell-find-hit-active`),
`a.wterm-link[data-kind]`. 250-row blocks, ≤2,000 held scrollback rows.

## Manifest edits the lead owns (batch them, one lockfile resolve)

- `crates/roost-web/Cargo.toml`: add `dioxus` (+ `dioxus-web` features as needed) —
  `main.rs` already calls `dioxus::launch` and `lib.rs` already uses `#[component]`.
- `crates/roost-web-terminal/Cargo.toml`: `web-sys` (R1 owns this one).
- `crates/roost-client-core/Cargo.toml`: `[dev-dependencies] roost-coord/roost-worker/roost-keeper`
  at the U-1c commit, for the integrator's Phase 4 `headless_client.rs`.
