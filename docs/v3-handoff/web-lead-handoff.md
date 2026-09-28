# Track U — web lead handoff

Worktree `/home/almalinux/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -20 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".
Lead: `WebLeadU3` (successor of `WebLeadU2`, `WebLeadU`).

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

1. **Lint, 2 violations** (design raw-value ratchet): `crates/roost-web/src/smoke/paint_proof.rs:162`
   (`starts_with("rgba(")`, parsing a computed background) and
   `crates/roost-web/tests/smoke_scans.rs:299-308` (test inputs). v2's lint never
   saw these (it skips `*.test.ts`; v2's harness spells the check as a regex
   `/^rgba\(…/`). Decide with the integrator (xtask owner): exempt `tests/` +
   `src/smoke/` from `design_raw`, or baseline them. Do not disguise the literals.
2. Finish the gate: two agreeing three-crate test runs, wasm32 build
   (`cargo build -p roost-client-core -p roost-protocol -p roost-web -p roost-web-terminal --target wasm32-unknown-unknown`),
   lint 0; then report to Main.
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
