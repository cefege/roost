# Track U — web lead handoff

Worktree `/home/almalinux/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -12 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".
Lead: `WebLeadU2` (successor of `WebLeadU`).

## Build rule

`source /tmp/webenv.sh && c <cargo args>` — wraps every cargo/dx call as
`flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …`
with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=target-track`,
no RUSTFLAGS, and a disk check (< 10 GiB → refuse). Recreate `/tmp/webenv.sh`
from this line if `/tmp` was wiped. Never end a turn while a build runs.
Slice brief: `docs/v3-handoff/web-slice-context.md` (copy of `/tmp/web-slice-context.md`);
wave-B interfaces: `docs/v3-handoff/web-waveB-contract.md`.

dx bundles are built from a detached worktree of a committed SHA,
`target-track/dx-tree` (`git worktree add --detach target-track/dx-tree <sha>`),
sharing `target-track`, so a bundle never picks up a slice's half-written file.

## Decisions

- **Layout adapter, one owner**: `crates/roost-protocol/src/proto_adapters/layout_document_proto.rs`
  (port of v2 `packages/protocol/src/layout-document-proto.ts`) is THE
  implementation, incl. `canonical_layout_document` (v2 `handlers-ui.ts:66,120`
  composition). Committed alone as `056885b6`. The coord lead deletes coord's
  duplicate `crates/roost-coord/src/ui_state/layout_proto.rs` and repoints
  `ui_state/rpc.rs` + `tests/ui_state_legacy_command.rs`; verified locally
  (repointed, then reverted): `cargo test -p roost-coord --test ui_state_legacy_command`
  = 7 passed / 0 failed.
- **roost-web → roost-platform** allowed (v2 `web → @roost/platform` edge, for
  `lib/nativePath.ts`): one line in `xtask/src/crate_dag.rs` (cross-owner),
  one impl `crates/roost-web/src/platform/worker_paths.rs::BrowserWorkerPaths`.
- `browser/browserPlatform.ts`: SHELL ports the pure shortcut matcher into
  `crates/roost-web/src/platform/browser_platform.rs`; the BROWSER row extends it.

## Done (committed)

| Item | Commit |
|---|---|
| U-0 items 1–3 | merged into `v3` at `d1258917` |
| U-1 DECODE | `dcfd84bb` (694/0/3 ×2, gate green) |
| Layout adapter (shared) | `056885b6` |
| PUMP (client-core hydration/redial/unary + roost-web pump) | `a5a10810` |
| UiCommand | `cb19ca85` |
| GamepadTv (input_nav, terminal_nav_pad) | `c7fe56b2` |
| MdDesignTheme (md, /design, theme, assets) | `d3628b5b` |
| RendererInput (input controller, selection, mouse, echo host) | `a61c544c` |
| Layout test import fix; pad hints split; Dioxus.toml for dx 0.7 | `50d6153f`, `d8495e79`, `bf7cfe2c` |
| Renderer core (RenderElement seam, rows, history, find, links, scheduler) | `bdbdfead` |
| PUMP live fixes (wall-clock JWT, monotonic sweep, wasm timer panic) | `fb9c63f2` |
| predictive_echo grid fix restored (a61c544c caught a mutant) | `2303782f` |

**PUMP live check: PASSED at `2303782f`.** Bundle built from `target-track/dx-tree`
(`dx build --release -p roost-web --platform web`; output lands in
`target-track/dx/roost-web/release/web/public`, staged to `target-track/dist-pump`).
Probe `target-track/probe/stack-probe.ts` (boots the smoke stack with
`ROOST_SMOKE_WEB_DIST`, mints a browser token, opens `/#pair=` in Playwright
Chromium, samples testids, dumps console/responses/ws counts and coord auth log
lines): redeem 200 → reload → one sync socket → six list RPCs once →
`.workbench-shell[data-compact="false"]` + home landing, no pageerror.
(`browser` tool daemon was unavailable; Playwright from `node_modules` works.)

Mutations for wave A + PUMP: `'/home/almalinux/.omp/agent/sessions/-repos-roost/2026-09-28T03-31-17-220Z_01a0e611-2064-7034-8e32-629265a564ea/WebLeadU2/WebLeadU2.WebWaveAMutate.md'` (`/mutations`,
34 entries; all fail as expected except three PUMP invariants with no guard —
terminal hydration → Authorized, no DomainReady on the Err path, frame resets
redial failures — being guarded by WebPumpGuards).

Before these commits the whole tree tested 1314 passed / 8 failed / 4 ignored:
3 routable fixture defects (fixed in the PUMP commit, v2 `sync-routable.ts:5`)
and 5 `terminal_links` failures in the cancelled renderer-core port
(WebRendererCore2 is fixing them).

## Uncommitted wave B — snapshot `refs/heads/v3-web-snap-waveb2` = `06e3320a`

Restore on `f85bde98`: `git checkout v3-web && git stash apply 06e3320a` (122 status
lines). Every slice stopped at budget; reports (read them first):
`agent://WebLeadU2.{WebShell,WebSidebar,WebDeck,WebDeck2,WebTerm,WebTerm2,WebSmoke,WebPumpGuards}`.

| Slice | State |
|---|---|
| SHELL | routes/app/router_state rewritten; AppShell `.workbench-shell[data-compact]`, MainPane (dead-route net), AppErrorBoundary, RenameDialog, context_menu, motion/*, keyboard_shortcuts, browser_platform matcher. ~60 tests written, NOT run. Not done: UiBridge, spawnSession/killAfterUndo (`sweep_pending_closes` → SessionsKill has no caller). |
| SIDEBAR | SidebarRoot/FolderList (`data-testid=folder-list`) mounted in AppShell; client-core `store/sidebar/*` (19 tests pass); `BrowserWorkerPaths`; roost-web tests not run. Cross-owner: `xtask/src/crate_dag.rs` roost-web → roost-platform; workspace `unicode-segmentation`. |
| DECK | client-core `deck/*` done + tested (3 mutations); `TerminalDeck` written and mounted in MainPane (WebDeck2); deck_spawn 3/0. Drafts status: see WebDeck2 transcript. |
| TERM | CellGridRenderer mounted, pane registry, input/echo/selection/mouse/links/cursor-poll wired; find bar, nav buttons, context/capture menus NOT done. |
| SMOKE | `smoke/**` behind `feature="smoke"`: all **53** SmokeApi members (smokeTypes.ts:118-287 has 53, not 56 — counted), 43 real, 10 reject naming their slice (TERMINAL DIAG ×2, STREAM LIFECYCLE, BROWSER ×5, CARRIER/LOCAL, ATTACH). |
| PUMP guards | NOT written (see below). |

Known compile break (wasm32 only): `components/context_menu/dom.rs:57`
(`Element::is_connected` → needs `Node` API / web-sys feature) and
`motion/grid_flip.rs:48` (`HtmlElement::dataset` → web-sys `DomStringMap`
feature). Native `--all-targets` compiled at the end of WebSmoke's run.

## Exact next steps

1. Restore the snapshot; fix the two wasm32 errors; `c check` native + wasm32
   (`--features smoke` too); rustfmt (split anything > 400); run all three crates'
   tests; classify reds by v2; commit per slice (shared registration files go in
   the first commit that needs them; build the dx bundle from a committed SHA in
   `target-track/dx-tree` to prove the series compiles).
2. PUMP: three unguarded invariants (WaveAMutate survey) — terminal hydration →
   `Authorized`; no `DomainReady` when `mark_domain_ready` refuses; a frame resets
   redial failures. Live defect: a refused Sync upgrade (HTTP 401) redials every
   ~250 ms; leading cause (WebPumpGuards): `resume_requested` is cleared only in
   `SyncRedial::take_due`, so the boot `PageVisibilityChanged{visible}` wake
   latches it and every refused close redials at once (v2 clears it in
   `_waitForSyncDialPermission` before every dial). Consequence: the 3 s bootstrap
   probe never fires, so a revoked device stays at `Checking`. Also: the pump drops
   the close reason (1013 backpressure is not an immediate redial as v2 `flow`).
3. `tests/route_surfaces.rs` asserts the pre-SHELL mapping and will fail — update
   to v2 `App.tsx` routes (`tests/route_session.rs` has the new mapping).
4. dist-smoke bundle (`dx build --release -p roost-web --platform web --features smoke`)
   → `terminal-delivery.spec.ts` "browser smoke flow creates and cleans its resources";
   production bundle → `grep -rc __smoke` = 0.
5. Remaining U-2 rows: PAIRING, SETTINGS (lazy audit hydrator un-ignores the
   `sync_decode_routable` audit case), BROWSE, MACHINES+AGENTS, SEARCH+PALETTE+HELP,
   NOTIFICATIONS+PUSH, COMPOSER+VOICE, BROWSER platform, SYNC LIFECYCLE,
   STREAM LIFECYCLE, CARRIER, LOCAL, ATTACH, TERMINAL DIAG; TERM remainder
   (find bar, nav buttons, context/capture menus); DECK remainder.
6. Track gate (not run since `dcfd84bb`).

Slices finish ~1 h of work each before their budget ends; keep briefs to one
component each and require a compiling stop.
