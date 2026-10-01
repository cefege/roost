# Track U — web lead handoff

Worktree `/home/mike/repos/roost-v3-web`, branch `v3-web`. The worktree
(`git log --oneline -20 && git status --short`) is the state; this note is the
moment it was written. Plan: `roost-v3-finish-and-cutover-plan.md` "### Stage 5".
Lead: `WebLead3` (host `/home/mike`, successor of `WebLead2`).

## Current state (WebLead3, read this first)

Tip `77b4e38b` "web-terminal, web: the wasm32 half of the UI crates is inside a
lint at last" — pushed, `origin/v3-web` = the same SHA, tree clean. Three
commits this session, all gated:

1. `f17b305c` — the preserved WIP finished: the TERMINAL DIAG half of U-2. It is
   byte-identical to `origin/v3-web-snap-resume2` (`dabfd789`) except three fixes
   the snapshot needed to compile and to stop lying: `crates/roost-web/src/smoke/stream_probe.rs`
   called a `string_member` that did not exist;
   `harness_host.rs`'s `FlowHost::terminal_stream_probe` answered
   `unported_refusal("terminalStreamProbe")`, which for a now-ported member is
   `None` and produced an empty-string refusal; and one of the new tests was
   vacuous (its fixture named no session, so the error arm never ran and every
   member read `null` for free). The wire contract is under "The `__smoke` member
   the coordinator's X2 slice consumes" below — it is the only place it is
   written down.
2. `3489db11`, `654bd72a` — this note.
3. `77b4e38b` — **run this gate from now on, it is the one that was missing:**
   `cargo clippy -p roost-web --features smoke --target wasm32-unknown-unknown
   --all-targets -- -D warnings`. It was red on **13** findings (3 in
   `roost-web-terminal`, 10 in `roost-web`) and no CI step could see any of them,
   because `ci.yml`'s wasm32 step is `cargo build` and every wasm-only file is
   skipped by the host clippy, by host `--all-targets` and by `cargo test`. It is
   now exit 0. The tree passed BOTH the host workspace clippy and the CI wasm32
   `cargo build` the whole time all thirteen sat there, which is
   `docs/v3-handoff/silent-no-ops.md` #4 with a fresh example.
   `TerminalInputOptions` moved to `input/options.rs` in that commit: the lint's
   type alias pushed `controller_dom.rs` to 403 lines against a 400 baseline, and
   the honest fix is the concept split the file was already asking for.

Gate on `77b4e38b`:

- `cargo clippy -p roost-web --features smoke --target wasm32-unknown-unknown
  --all-targets -- -D warnings` → **exit 0** (was 13 errors).
- `cargo clippy --workspace --all-targets --keep-going -- -D warnings` → exit 0.
- `cargo nextest run -p roost-client-core -p roost-web -p roost-web-terminal --no-fail-fast`
  → **1344 passed / 4 skipped**. Run this one too: CI's `cargo test --workspace`
  never enables the `smoke` feature, so every `#![cfg(feature = "smoke")]` test
  file compiles to nothing there (silent-no-ops #5).
- `cargo nextest run -p roost-web --features smoke --no-fail-fast`
  → **387 passed / 0 skipped**.
- `ROOST_REPO_ROOT=$PWD cargo xtask lint` → **3988 inputs, 0 violations**.
- `cargo xtask fmt` → clean, confirmed by `git status --short` (its success word
  is "formatted", which asserts nothing — silent-no-ops #11). For the crates you
  touch use `cargo fmt -p <crate>`; bare `rustfmt --edition 2024 <file>`
  disagrees with `cargo fmt` and produces a red fmt check.
- `cargo build -p roost-client-core -p roost-protocol -p roost-web
  -p roost-web-terminal --target wasm32-unknown-unknown` → exit 0; `cargo check
  -p roost-web --target wasm32-unknown-unknown` with and without
  `--features smoke` → exit 0.
- No test was added or changed for the lint commit: nothing in it is a behaviour
  change, and a test that cannot fail on any of it would be a test of the
  formatter.

## U-2 PAIRING — scoped, NOT started. Start here.

The budget went on finishing the WIP and closing the lint hole. PAIRING is a
~1,900-line row and roughly 40% of it is already landed, which makes it a trap for
the next session that reads the plan and starts porting. What is ALREADY DONE —
do not re-port it:

- `browser/browserSelfLabel.ts` → `crates/roost-web/src/platform/self_label.rs`
  (129 lines, unit-tested; already consumed by `pump/boot.rs`).
- `lib/pairedBrowserNotice.ts` → `sync/inbound/payloads.rs:113`
  (`PairedBrowser::announcement_label`) + `handle_sync/fold_controls.rs:88` and
  `ANNOUNCED_PAIRINGS_MAX` in `store/sync_feeds.rs:39`. The dedupe is done.
- `components/pairing/AccessCheckingScreen.tsx` → `components/access_gate.rs:31`
  (`CheckingScreen`, `UnauthorizedScreen`). The v2 file is a DROP.
- The whole ceremony value-machine → `roost-client-core/src/client/auth/`
  (`pairing_session.rs`, `pairing_approval.rs`, `ceremony_store.rs`,
  `pairing_requests.rs`, `redeem.rs`). Timer-free by design
  (`pairing_session.rs:12-16`): the row's work is the HOST.
- The three pairing stylesheets are already in
  `crates/roost-web/assets/components/pairing/`.
- Every `md/` primitive the row composes from is ported: `Surface`, `StatusDot`,
  `Button`, `Card` (already takes `title`/`supporting`/`trailing`/`test_id`),
  `List`/`ListRow`, `Chip`, `Dialog`, `TextField`, `EmptyState`, `SectionTitle`.

`Route::Pair` already exists (`routes.rs:74`); `app.rs:61-77` deliberately maps it
into the `NotServed` arm, so `/pair` renders `not_served::NotServed` today. The
cutover is a new `ServedSurface::Pair` moved OUT of that arm, plus a `RouteContent`
arm, plus `in_shell()` returning `false` for `Pair` (the arm `Design` already
takes). Three existing tests will then be WRONG, not incidental, and get
  re-pinned: the two in `app.rs` and `tests/route_surfaces.rs:29,38`.

Order, every step compiling (★ = independently committable):

1. `roost-client-core/src/client/auth/pairing_transient.rs` —
   `is_transient_pairing_error(&CallError)`. The whole 19-line v2 file is the
   remainder; `ConnectCode` already has all five wire names and `CallError::code()`
   already returns `None` for exactly the non-Connect arms. ★
2. `client/auth/pair_approval_lifecycle.rs` + `pair_approval_resolve.rs` — the
   largest genuine remainder: `classifyPairRpcFailure`, `parseApprovalStatus`,
   the two evidence enums, `resolveApprovalStatus`, `resolveCancellation`, the
   step/outcome/status enums, `PAIR_APPROVAL_OUTCOME_TOASTS`. **The load-bearing
   rule is the `default` arm**: a stale ceremony version, `Unimplemented` and any
   unclassified refusal map to `reload`, NOT to `authority` — collapsing them
   shows "Pairing authority is no longer valid" to a client that only needs a
   reload, and destroys the approver's code. `PairApprovalStatus` is a 5-value
   subset of the ported `PairPollStatus`; express it as a subset, never re-declare
   it. `resolveCancellation` has the mirror rule: a denial whose RESPONSE was lost
   must fall through to a status read, because the row may already be gone
   precisely because that denial committed. ★
3. `client/rpc/calls/pairing.rs` — six `UnaryMethod` impls (`PairCreate`,
   `PairPoll`, `PairConfirm`, `PairApprove`, `PairDeny`, `PairApprovalStatus`).
   No new transport, no `RpcCall` variant, no `rpc/methods.rs` change:
   `CoordRpc::call` is already generic. Everything after this is host work. ★
4. `components/pairing/` leaves: `status_notice.rs`, `page_header.rs`,
   `verification_code_dialog.rs`, `request_card.rs`, `pair_request_card.rs`
   (the approver card; `is_pair_request_expired` is pure and lives in it with
   `#[cfg(test)]`), `other_options.rs`. All presentational, all ★. Add
   `pub mod pairing;` to `components/mod.rs`.
5. `pairing/ceremony/{operation,controller,host}.rs` — the requester. MUST be
   three files: v2's `onboarding-pairing-ceremony.ts` is 393 lines and a faithful
   port with the fence and the retry schedule is 500-600. The testable unit is
   the FENCE, which is what all 8 `onboardingPairVerification` cases assert. Not ★.
6. `pairing/approval/{operation,controller,host}.rs` — the approver, same split
   (`PairApprovalProvider.tsx` is 370 and ports to 500+). `armExpiry` is
   pre-acknowledgement only, and the replay-on-restore effect is a real
   requirement, not polish. Not ★.
7. `pairing/{requester_provider,gate_panel,approver_list,surface}.rs` + the
   `Route::Pair` cutover. `Onboarding.tsx`'s wheel/touch scroll handling goes in
   its own `scroll_owner.rs` so the presentational half stays native.
8. Specs: `pair-gate`, `pair-verification`, `local-first-onboarding`, and
   `tv-dpad.spec.ts` (it also asserts on `/pair`, so it is a fourth gate even
   though the plan names three). `design-reviewer` is mandatory on the diff.

BLOCKERS OWNED BY ANOTHER ROW — raise them, do not work around them:

- **No component can raise a toast.** `store::toasts::add_toast` exists
  (`store/toasts.rs:92`) but is reachable only from inside `ClientCore::handle`,
  and grep for `add_toast|ToastKind` under `crates/roost-web/src` returns ZERO
  hits. Every pairing outcome is a toast in v2 (14 call sites). Needs either a
  generic `ClientEvent` toast variant (NOTIFICATIONS row) or a thin host-side
  `store.toasts::raise` shim. Do NOT declare a toast store inside pairing — that
  is the two-implementations-of-one-value defect `CLAUDE.md` §12 exists to stop.
- `PairRequestNotifier.tsx` renders into the notification dock, which has no Rust
  owner yet (`assets/components/notifications/NotificationDock.css` is present
  and unreferenced). Droppable in this row — the card and deny/approve are
  covered — but it is a cross-row call, not a local decision.
- No `DeviceKeyProbe` impl for the browser (`PairingOtherOptions`'s
  `isResetWebKeyEligible`); the trait and `WebDeviceKey::reset` are ported, the
  host adapter is a one-method write.
- `pump/boot.rs:105-116` duplicates `redeem.rs::RefusalCode::is_authoritative`
  rather than using it, so the authoritative set is spelled twice. Worth
  collapsing in this row, since `other_options` needs the redeem path anyway.

Two `docs/FAILURE-INDEX.md` entries CONSTRAIN the port and are not bugs to fix:
L1449-1468 (a global key router must not claim bare ↑/↓/⏎ on a route with no
cursor, naming `/pair`; ⏎ must not `preventDefault()` a focused `<button>`) and
L1470-1487 (`<body>` counting as a D-pad origin on the unpaired gate). The Rust
equivalents already exist in `input_nav/spatial.rs`.

## Bundles

`crates/roost-web/dist-smoke/` is gitignored (`.gitignore:3`). `dx` does NOT clean
its output dir, so the two builds must be separated by an `rm -rf` of
`target-track/dx/roost-web/release/web/public` or the production copy inherits the
smoke wasm:

```
rm -rf target-track/dx/roost-web/release/web/public
dxb build --release -p roost-web --platform web --features smoke
rm -rf crates/roost-web/dist-smoke && cp -r target-track/dx/roost-web/release/web/public crates/roost-web/dist-smoke
rm -rf target-track/dx/roost-web/release/web/public
dxb build --release -p roost-web --platform web
rm -rf crates/roost-web/dist && cp -r target-track/dx/roost-web/release/web/public crates/roost-web/dist
grep -rc __smoke crates/roost-web/dist | awk -F: '{s+=$2} END {print s}'   # must be 0
```

TERM gate:
`ROOST_SMOKE_WEB_DIST=$PWD/crates/roost-web/dist-smoke bunx playwright test smoke/terminal/terminal-delivery.spec.ts -g "browser smoke flow creates and cleans its resources" --project chromium-desktop --reporter=line`
(`node` is not on PATH; use `bunx`. The fixture needs `.workbench-shell[data-compact]`,
one `[data-testid=folder-list]`, no error boundary, then `__smoke.runFlow`.)

Measured on this tip: the smoke bundle `grep -rc __smoke crates/roost-web/dist-smoke`
totals 6, the production bundle `crates/roost-web/dist` totals **0**; the TERM
gate passed 3 runs in a row (5.5 s / 3.5 s / 3.7 s) with `ROOST_SMOKE_WEB_DIST` as
the only override (stack: `coordinator=typescript worker=typescript`).

### The `__smoke` member the coordinator's X2 DiagSnapshot slice consumes

`window.__smoke.terminalStreamProbe(sessionId)` — an `Answer::Promise` member. It
takes the browser layer (`terminalBrowserSnapshot`), sends it as
`DiagSnapshot { spa_state_json }`, and returns the normalized three-layer record
(`captured_at_ms`, `session_id`, `browser`, `coord.{build,session,terminal_control}`,
`worker.{worker_fp,status,response_ms,build,session,error}`). The coordinator
answers with `snapshot_json`; a missing or malformed layer stays explicit
(`status: "missing"`, `null`) rather than reading as healthy.

`window.__smoke.terminalBrowserSnapshot(sessionId)` is its `Answer::Sync` half and
answers without the round trip. `SMOKE_METHODS` is **53** (recounted against v2
`smokeTypes.ts:118-287`: 53 declared, 53 registered, 0 uncovered); both members
were already registered at `28eebcee` and this commit only removed their
`UNPORTED_METHODS` entries.

## Previous state (WebLead2)

Gated tip `659b50c8`:
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
- The TabId WIP that was NOT in that tip has since landed as `32120c5e` ("one tab id
  per document, arbitrated off a duplicated tab's"), so the SYNC LIFECYCLE remainder
  is that row's tail, not its `tab-id.ts` port.
- Build rule: `CARGO_INCREMENTAL=1 CARGO_BUILD_JOBS=3`, nextest for full suites.

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
