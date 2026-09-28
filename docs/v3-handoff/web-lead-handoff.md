# Track U — web lead handoff

Worktree `/home/mike/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -20 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".
Lead: `WebLead2` (host `/home/mike`, successor of `WebLead`).

## Current state (WebLead2, read this first)

Gated tip `659b50c8` (merge this):
- `3960743d` view-state installs its stream at the authority's effective geometry; UUID view ids.
- `e11c611e` republishing a view at its published size keeps its revision (the 1→2→3→4
  climb: v2 `refresh`/`changeIntent` keep it; wire showed rev=1 then rev=2 same payload).
- `659b50c8` insecure origin mints no view id (design-review P3).
- TERM gate on a clean `dist-smoke` of this tree: `1 passed` (three runs).
- `dist` rebuilt from a cleared folder: `grep -rc __smoke crates/roost-web/dist` total 0.
- Gate: `cargo nextest run -p roost-client-core -p roost-web -p roost-web-terminal --no-fail-fast`
  1333 passed / 4 skipped; `cargo test` same crates 192 suites, 1333 passed / 0 failed / 4 ignored;
  clippy `--workspace --all-targets --keep-going -D warnings` exit 0; lint 3958 inputs, 0
  violations; `cargo xtask fmt` then `git status --short` empty; CI wasm32 build exit 0;
  `cargo check -p roost-web --target wasm32-unknown-unknown` ±`--features smoke` exit 0.
- TabId WIP (SYNC LIFECYCLE) is NOT in the tip: it is in snapshot `v3-web-snap-term-viewstate`
  and the worktree stash `tabid-wip`.
- Build rule now: `CARGO_INCREMENTAL=1 CARGO_BUILD_JOBS=3`, nextest for full suites.

## Previous state (WebLead)

Gate part 1 on `97ba537c`'s tree — all green:
- tests ×2 + clippy: see WebLeadU4 below (1323/0/4 twice; clippy exit 0).
- `ROOST_REPO_ROOT=$PWD cargo xtask lint` → 3953 inputs, 0 violations.
- `cargo xtask fmt` then `git status --short` → empty.
- `cargo check -p roost-web --target wasm32-unknown-unknown` exit 0; same with
  `--features smoke` exit 0; CI line `cargo build -p roost-client-core -p roost-protocol -p roost-web -p roost-web-terminal --target wasm32-unknown-unknown` exit 0.

Bundles (scope item 2). `dx` does NOT clean its output dir: a second build
leaves the first build's hashed wasm/js beside its own, so a production copy
taken after a smoke build contains the smoke wasm (measured: `__smoke` total 6,
all in the stale `…dxhad5c…wasm` that `index.html` does not reference). Always:
```
rm -rf target-track/dx/roost-web/release/web/public
dx build --release -p roost-web --platform web --features smoke   # (under the build lock)
rm -rf crates/roost-web/dist-smoke && cp -r target-track/dx/roost-web/release/web/public crates/roost-web/dist-smoke
rm -rf target-track/dx/roost-web/release/web/public
dx build --release -p roost-web --platform web
rm -rf crates/roost-web/dist && cp -r target-track/dx/roost-web/release/web/public crates/roost-web/dist
grep -rc __smoke crates/roost-web/dist | awk -F: '{s+=$2} END {print s}'   # must be 0
```
Each dx release build ≈ 4-5 min once it has a slot.

TERM gate (scope item 3):
`ROOST_SMOKE_WEB_DIST=$PWD/crates/roost-web/dist-smoke bunx playwright test smoke/terminal/terminal-delivery.spec.ts -g "browser smoke flow creates and cleans its resources" --project chromium-desktop --reporter=line`
(`node` is not on PATH here; use `bunx`). First run on the `97ba537c` bundle: the shell
mounts, `worker_available`/`shell_painted`/`workspace_created` pass, then
`flow_exception: terminal transport is not connected` — no view was ever
published (fixed in `916387f8`, below). The probe layer reports "U-2 TERMINAL
DIAG … not ported" (expected until that row).

Commits this session: `a30c16bc` (.gitignore dist-smoke), `916387f8` (views
publish on the v2 publication target; republish on terminal-domain ready),
`8007ba32` (1013 backpressure close → immediate redial; closes open item 4b).

Host notes: `/home/mike/repos/webenv.sh` defines `c` (cargo) and `dxb` (dx)
under the build lock. Commits need `GIT_AUTHOR_*`/`GIT_COMMITTER_*` env
(Mihai Mateias <mateiasmihaiandrei@gmail.com>); no git identity is configured.
Build-slot waits reached 20 min in this session (all three tracks building).

`1c75b287`: closed tabs are killed after the undo window (v2 `killAfterUndo`; closes open item 4a).

### Stop state (budget), uncommitted work is in snapshot `v3-web-snap-term-viewstate`
TERM gate after `1c75b287`: input now routes, but `shell_round_trip` fails with "marker was not
visibly painted" (`baseline_ready:false, stream_id:null`). I captured the wire with a throwaway
WebSocket-decoding spec (deleted). It shows the view published and ACCEPTED, with a `streamId`
and a full `cellGrid` delivered, which isolates two client defects. Both fixes are in the snapshot,
NOT committed and NOT test-run:
1. `pane_mount.rs` minted `view-<hex>` view ids, and the coordinator refuses any non-UUID
   (`terminal-view-protocol.ts:65`). Fixed with `crypto.randomUUID()` (`pane_mount/browser.rs`
   `mint_view_id`). Design review APPROVED; P3 note: `Crypto::random_uuid` throws rather than
   returning None on insecure origins, so the doc comment overclaims (check `is_secure_context`).
2. `handle_correlated_result` hard-coded `stream_id: None`, and decode dropped the stream id and
   effective geometry, so an accepted view never installed its stream. Fixed:
   `SyncFrame::ViewState` and `ViewStateResult` carry `stream_id`/`effective_cols`/`effective_rows`,
   and the stream is installed at the authority's effective geometry after
   `is_terminal_uuid`/`is_terminal_geometry` (v2 `terminal-stream-view-commands.ts:205-216`).
   `cargo check -p roost-client-core --all-targets` exit 0. Needs a regression test: an accepted
   ViewState installs the stream, and a full frame then makes the replica paintable.
   Mutation: revert to `None`.
   Next: test, commit, rebuild dist-smoke (clean procedure), rerun the TERM gate.
   Also seen on the wire: the view revision climbs 1→2→3→4 on renewals every ~3-4 s. v2 renews
   the same revision, so check whether something other than the heartbeat bumps it.
3. SYNC LIFECYCLE `tab-id.ts` port (helper TabId, stopped mid-verification):
   `client/auth/tab_id.rs` (8/8 `tab_identity` tests green), `platform/tab_id.rs` (wasm, NEVER
   compiled), and `pump/boot.rs` claims before the first dial. It also edits `lib.rs`,
   `platform/{mod,rpc,connect}.rs` (tab id header becomes `RefCell` + `present_tab_id`).
   Next: wasm32 check with and without smoke, clippy, tests, and a mutation (the `Occupied` arm).
   Full notes are in the helper report (transcript `history://WebLead.TabId` on the old session).

## Pause state (WebLeadU4, historical)

Tree = `e09f39ca` + this doc commit. No slices were spawned; no services or
builds of this track are running. Gate on `e09f39ca`'s tree (scope item 1):

- DONE: `cargo test -p roost-client-core -p roost-web -p roost-web-terminal --no-fail-fast`
  twice, agreeing: 189 suites, **1323 passed / 0 failed / 4 ignored** (468 s, 235 s).
  Ignores: `does_not_resume_a_nonfinal_direct_upload_through_coordinator_status`
  (U-ATTACH), `audit_rows_are_newest_first_deduplicated_and_bounded` (SETTINGS),
  `refuses_a_ninth_simultaneously_demanded_browser_peer` and
  `accepts_a_bounded_ready_for_the_full_256_session_grant` (U-CARRIER).
- DONE: `cargo clippy --workspace --all-targets --keep-going -- -D warnings` → exit 0.
- Snapshot `v3-web-snap-pause` = `419149c6`: a stash commit on `5a43d383` (the dx-tree mutation
  worktree) holding the 4 dirty mutation-agent test files and, under
  `mut-artifacts/`, the local-only `target-track/tmp` material (the NOT-applied
  `mut-MutDeckSidebarCore.patch`, the MutShellSidebar 92-mutant `mutants.py` +
  `harness.py`, the MutDeckSidebarCore driver/specs/results, `mutation-brief.md`).
  Only the `store_sidebar.rs` change is new; the other three are already in the
  TERM/SMOKE `fixup!` commits. Never apply it wholesale.

## Build rule

`source /tmp/webenv.sh && c <cargo args>` — wraps every cargo/dx call as
`flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …`
with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=target-track`,
no RUSTFLAGS, and a disk check (< 10 GiB → refuse). Recreate `/tmp/webenv.sh`
from this line if `/tmp` was wiped. The host has TWO build slots shared with the
worker/coord tracks: a 1-minute build routinely waits 3-8 min for a slot.
Never end a turn while a build runs: block with an eval that runs
`flock target-track/.roost-build.lock true` (the bash tool auto-backgrounds).
Use `cargo clippy … --keep-going` so one failing crate does not hide the rest.
Formatting: `rustfmt --edition 2024 <crate>/src/lib.rs` (no build slot) — it does
not format inside `rsx!`.

## Done (committed on `v3-web`)

| Item | Commit(s) |
|---|---|
| U-0, U-1 DECODE, PUMP, UiCommand, GamepadTv, MdDesignTheme, RendererInput, renderer core | see `git log` before `04c93348` |
| Pump listener holder wasm-only (clippy) | `pump` commit |
| xtask: roost-web → roost-platform edge; allowlist tests moved to `crate_dag/tests.rs` (cross-owner) | `xtask:` commit |
| roost-platform: Windows drive crumb named `C:` as v2 (cross-owner) | `platform:` commit |
| Wave B: SHELL, SIDEBAR, DECK, TERM, SMOKE (one commit each; shared registration files whole in SHELL; the series builds at SMOKE) + their `fixup!` commits (clippy fixes, strengthened tests; pushed unsquashed) | `web: SHELL …` … `web: SMOKE …`, `fixup! …` |
| PUMP invariants guarded + live redial defect fixed (`note_dial_started` before every dial, v2 `_waitForSyncDialPermission`); red first `[250,2000,4000]` → `[1000,2000,4000]`; 4 mutants all killed | `client-core: a pending resume is spent …` |
| Workspace clippy findings (web-terminal `FocusRefused`, aliases) | `web-terminal: a refused focus …` |

## Gate evidence (this lead)

- `cargo clippy --workspace --all-targets --keep-going -- -D warnings` → exit 0
  (after the fixes above, on the tree before the TERM/SMOKE test patch; that patch
  only strengthens three tests and deletes one).
- Tests: whole three-crate run on the wave-B tree 1318 passed / 0 failed / 5 ignored
  (+ `--features smoke` roost-web run green); after the redial fix client-core
  570/0/4. NOT yet: two agreeing runs of
  `cargo test -p roost-client-core -p roost-web -p roost-web-terminal --no-fail-fast`
  on the final tip.
- `ROOST_REPO_ROOT=$PWD cargo xtask lint`: 3954 inputs, **2 violations** (open, below).
- `cargo xtask fmt` clean; `git status --short` clean.
- wasm32: `cargo check -p roost-web --target wasm32-unknown-unknown [--features smoke]`
  clean on the wave-B tree; not re-run after the web-terminal `FocusRefused` change
  (`controller_dom.rs` is wasm-only).

## Mutations

- TERM + SMOKE survey (`agent://WebLeadU3.MutTermSmoke`, 77 mutants): all failed
  their guard; 3 survived and their tests were strengthened (smoke_input_observer
  route_retirement, terminal_dom_repair target retirement, viewport_publication
  grace absorption), applied in the TERM/SMOKE fixups; the wiring-only
  `the_redial_report_reads_an_open_link_with_no_failures` was deleted.
- Earlier slice runs: DECK 3 (route_selection early return, undo_close commit,
  warm-set cap), SIDEBAR 1 (cursor clamp), SMOKE 6, TERM 2 (see WebLeadU2 reports).
- SHELL + SIDEBAR(web) survey `agent://WebLeadU3.MutShellSidebar` and DECK +
  SIDEBAR(core) survey `agent://WebLeadU3.MutDeckSidebarCore` were stopped at the
  budget: read their yields; any strengthened tests are in
  `target-track/tmp/mut-<name>.patch` (apply with `git apply --3way`, then run the
  touched test targets). Mutation worktree: `target-track/dx-tree` (detached at
  snapshot `5a43d383`).

- Final state of the two stopped surveys: MutDeckSidebarCore ran its batches
  (list in its yield); its strengthened `store_sidebar.rs` test is in
  `target-track/tmp/mut-MutDeckSidebarCore.patch`, NOT yet applied. MutShellSidebar
  ran only the no-mutant baseline (135/0); its 92-mutant catalogue and harness are
  in `target-track/tmp/mut-MutShellSidebar/` (`flock target-track/.mut.lock
  python3 harness.py b1`…`b9`), with 8 predicted survivors listed in its yield.

## Open items, in order

1. ~~Lint, 2 violations~~ — RESOLVED by the integrator in `e09f39ca` (the design
   ratchet skips test files as v2 does; the `paint_proof.rs` colour parser line is
   baselined): 3953 inputs, 0 violations.
2. Finish the gate: tests ×2 and clippy DONE (Pause state); still to run: lint,
   fmt + `git status --short`, wasm32 build/check with and without `smoke`; then
   report to Main.
3. Step 4 (not started): `.gitignore` needs `crates/roost-web/dist-smoke/` (only
   `dist/` is ignored). Build `dx build --release -p roost-web --platform web --features smoke`
   (output lands in `target-track/dx/roost-web/release/web/public`) → copy to
   `crates/roost-web/dist-smoke`; same without `--features smoke` → `crates/roost-web/dist`;
   `grep -rc __smoke crates/roost-web/dist` must total 0. Then
   `ROOST_SMOKE_WEB_DIST=$PWD/crates/roost-web/dist-smoke node_modules/.bin/playwright test smoke/terminal/terminal-delivery.spec.ts -g "browser smoke flow creates and cleans its resources" --project chromium-desktop`
   (TS backend; the fixture needs `.workbench-shell[data-compact]`, one
   `[data-testid=folder-list]`, no error boundary, then `__smoke.runFlow`).
4. Known product gap (SHELL remainder): nothing turns
   `store::pending_close::sweep_pending_closes` into `SessionsKill` — a tab closed
   from the deck/sidebar is never killed on the worker (v2 `closeSession.killAfterUndo`).
   Also: the pump drops the close reason, so a 1013 backpressure close is not an
   immediate redial (v2 `flow`).
5. U-2 rows: PAIRING, SETTINGS, BROWSE, MACHINES+AGENTS, SEARCH+PALETTE+HELP,
   NOTIFICATIONS+PUSH, COMPOSER+VOICE, BROWSER platform, SYNC LIFECYCLE, STREAM
   LIFECYCLE, CARRIER, LOCAL, ATTACH, TERMINAL DIAG; TERM remainder (find bar, nav
   buttons, context/capture menus, file links, file drop); UiBridge.

## Snapshots

`v3-web-snap-waveb2` (06e3320a, WebLeadU2), `v3-web-snap-waveb3` (5a43d383,
compiled wave B before commits), `v3-web-snap-waveb3-commits` (fa90c596, the
slice commits before fixups).
