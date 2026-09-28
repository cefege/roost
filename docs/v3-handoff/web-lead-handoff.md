# Track U — web lead handoff

Worktree `/home/almalinux/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -5 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".

## Build rule

`source /tmp/webenv.sh && c <cargo args>` — wraps every cargo/dx call as
`flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …`
with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=target-track`,
no RUSTFLAGS (recreate the script from this line if `/tmp` was wiped). Never
end a turn while a build runs. Shared slice brief: `/tmp/web-slice-context.md`
(also snapshotted below).

## Done (committed, pushed)

| Item | Commit | Evidence |
|---|---|---|
| U-0 items 1–3 | merged into `v3` at `d1258917` | — |
| U-1 DECODE (+ RPC/Sync client codecs, WebDeviceKey) | `dcfd84bb` | 694/0/3 twice (3 named ignores), clippy workspace 0, lint 0 (2955 inputs), fmt clean, wasm32 build 0; 2 mutations in the commit body |

## In flight — UNCOMMITTED, snapshot `refs/heads/v3-web-snap-u1pump` = `1c215567`

Restore: `git checkout v3-web && git stash apply 1c215567` (on `f9287f24`).
Verified after the snapshot: `c check -p roost-client-core --all-targets` 0 errors, 0 warnings (core PUMP changes + rewritten fixtures). roost-web NOT yet compiled: roost-web-terminal slices were mid-edit (their errors, not the pump's).

**Lead's PUMP work (never compiled end to end yet):**
- client-core: `client/rpc/unary.rs` (UnaryMethod, ConnectCode/ConnectError/CallError, tests), `client/rpc/calls/{mod,sessions,workspaces}.rs`, `codec.rs` owns `encode_message`/`decode_message`; `sync/{redial,hydration}.rs` (v2 sync-redial/sync-watchdog policy, hydration tickets/retry/deadline/probe, tests); `handle_sync/{hydration,lifecycle}.rs` (subscribed → per-domain hydration RPCs → rows applied → `SendSync(DomainReady)` → ready; terminal publish → Authorized + drain retained; device rejection → Unauthorized; 4001 before open; redial loop, stale watchdog, lifecycle wakes, transport controls); `store/root.rs` (`CoordIdentity`, `mark_browser_device_rejected`, `mark_protected_snapshot_published`); Store gains `coord_identity`, `terminal_nav_pad` (GamepadTv slice owns the module). Removed dead paths: `SyncFrame::DomainReady`, `Effect::HydrateDomain`, `ClientEvent::HydrationCompleted`, `Effect::SignChallenge`/`ChallengePurpose`/`ClientEvent::ChallengeSigned` (no producer). `RpcResult::Failed { call_id, error: CallError }`, `RpcResult::{call_id,kind_name}`. New events `PageVisibilityChanged`, `SyncWakeRequested`, `SyncTransportControl`. Subscribed no longer sends Subscribe (v2 parity: only lazy audit subscribes).
- tests: `tests/support/hydration.rs` (fixtures now reach ready through the real hydration path); `support/sync_reconnect.rs`, `sync_decode_support/mod.rs`, `sync_domain_reset.rs` rewritten; `sync_decode_routable.rs` audit test `#[ignore = "SETTINGS: … lazy hydrator"]`.
- roost-web: `pump.rs` + `pump/{boot,socket,effects,browser}.rs` (dispatch queue, revision signal bumped only when `store.revision()` moved, socket notify-driven drain, effect executor, boot: key → identity → `#pair=` redeem → dial, 250 ms sweep + visibility/lifecycle listeners), `platform/{connect,self_label}.rs`, `platform/location.rs::replace_location`, `platform/sync_socket.rs` open takes `notify`, `lib.rs` App builds the pump, `app.rs` GatedApp reads `use_store()`. roost-web + roost-web-terminal Cargo web-sys feature lists broadened (lead).

**Wave A slices (task agents, uncommitted in the same tree):** WebMdDesignTheme (components/md, design, theme, assets, index.html), WebRendererInput (echo/input/mouse, client/predictive_echo, client/input), WebRendererCore (CellGridRenderer made generic over `RenderElement`, wasm-gated default impl; cell_renderer/scheduler/backfill/find/links), WebUiCommand (client/ui_state, client/ui_command; adds `SyncCommand::UiApplyLayoutResult`, `calls/ui_state.rs`, roost-protocol `proto_adapters/layout_document_proto.rs` — cross-owner), WebGamepadTv (roost-web input_nav, client-core store/terminal_nav_pad). Cross-owner edits seen in the tree: `Cargo.lock` (+1 dep line), `docs/phase4-client-contract.md`.

## Exact next steps

1. Compile: `c check -p roost-client-core -p roost-web --all-targets` + wasm32; fix; `c test -p roost-client-core -p roost-web -p roost-web-terminal`.
2. PUMP check: `dx build` → `ROOST_SMOKE_WEB_DIST=crates/roost-web/dist bun smoke/terminal/live-stack.ts` → browser leaves `Checking`. Commit PUMP (mutations: hydration publish, revision bump).
3. Collect wave-A slice reports, gate, commit per slice.
4. TERM mount (components/terminal port + session-keyed pane registry), SMOKE (56 methods, split by v2 smoke file), then wave B U-2 slices (SHELL, SIDEBAR+DECK — the fixture needs `.workbench-shell[data-compact]` and `folder-list` — PAIRING, SETTINGS incl. lazy audit, BROWSE, MACHINES+AGENTS, SEARCH+PALETTE+HELP, NOTIFICATIONS+PUSH, COMPOSER+VOICE, BROWSER platform, STREAM LIFECYCLE, CARRIER, LOCAL, ATTACH, TERMINAL DIAG).
5. Port audit: `/tmp/v2-port-audit.sh -v` counted 346/452 v2 modules (46,526 lines) not named in any Rust `//!` header at `f9287f24`.
