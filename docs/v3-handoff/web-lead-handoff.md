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

## In flight (agents of WebLeadU2, uncommitted in the tree; snapshot `v3-web-snap-waveb`)

- WebRendererCore2 — finish renderer core (roost-web-terminal cell_renderer,
  render_element, scheduler, backfill, find, links, startup_progress,
  terminal_presentation; client-core `search.rs`, `terminal/{frame_fold,history_backfill}.rs`
  + their tests). Commit as "web-terminal: renderer core …" after its report.
- WebWaveAMutate — guard mutations for PUMP/UiCommand/GamepadTv/MdDesignTheme/
  RendererInput; results go into the next handoff commit body.
- Wave B: WebTerm (components/terminal + pane registry), WebSmoke (56 SmokeApi
  methods), WebShell (routes/app/layout/MainPane/…), WebSidebar, WebDeck.

## Exact next steps

1. Collect reports; gate; commit per slice (path-restricted), mutations in bodies.
2. PUMP live check: dx bundle from `target-track/dx-tree` →
   `ROOST_SMOKE_WEB_DIST=<dist> bun smoke/terminal/live-stack.ts` → browser leaves `Checking`.
3. `dist-smoke` bundle (`--features smoke`) → `terminal-delivery.spec.ts`
   "browser smoke flow creates and cleans its resources".
4. Remaining U-2 rows: PAIRING, SETTINGS (incl. lazy audit hydrator — un-ignores
   `sync_decode_routable` audit case), BROWSE, MACHINES+AGENTS, SEARCH+PALETTE+HELP,
   NOTIFICATIONS+PUSH, COMPOSER+VOICE, BROWSER platform, SYNC LIFECYCLE,
   STREAM LIFECYCLE, CARRIER, LOCAL, ATTACH, TERMINAL DIAG, ASSETS remainder.
5. Port audit: `/tmp/v2-port-audit.sh -v` counted 346/452 v2 modules not named
   in any Rust `//!` header at `f9287f24` (before wave A).
