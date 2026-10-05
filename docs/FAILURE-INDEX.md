# Failure index

This file is the repo's institutional memory: every entry is a failure that actually shipped here, was
diagnosed, and was fixed — the wrong pattern is recorded next to the right one so it cannot be re-derived from
scratch.
It is grep-first, never read top-to-bottom: one `### ` heading per failure class, and the `**Symptom**` line
carries the report's own words, so grep this file for what the user said before you touch code.
Each `**Guard**` names the test, smoke spec or `cargo xtask lint` check that pins the entry; a `**Guard**` of
`none` means nothing stops a regression but this page.

---

## Store subscription and component lifecycle

### SPA projector hand-mirrors the shared event fold

**Symptom** — "SPA store doesn't reflect a SessionEvent variant / coord and SPA projections disagree (stale channel)"

**Wrong** — re-implementing the event switch in `store/projector.ts` as a hand-mirror of `foldEvent` from
`@roost/protocol/wire` (drifts — dropped `respawned`).

**Right** — `foldEventIntoStore` DELEGATES to shared `foldEvent` over the affected map slice, then diffs per-key
into the Solid store. No projector switch.

**Guard** — the fold is `crates/roost-protocol/src/wire/event.rs`'s single `fold_event` /
`all`, and `crates/roost-protocol/tests/session_fold_properties.rs` pins both
halves of the projection property —
`folding_incrementally_is_the_same_projection_as_folding_in_one_pass` for
determinism and incremental-equals-batch, and
`a_mutating_event_never_touches_the_map_it_was_handed` for the caller-must-not-
cache-a-stale-reference half.

### A component that renders store state through `use_pump` never repaints

**Symptom** — "clicking a tab does not make it active" / "the tab wrapper keeps
`data-active="false"` after the click" / "the compact badge still counts 1 after
a second terminal is open"

**Wrong** — `use_pump()` in a component whose render body reads
`pump.core().borrow().store()`. `use_pump` hands back the same
`Rc<RefCell<ClientCore>>` as `use_store` and differs by ONE thing: it does not
read `pump.revision()`. A `RefCell` behind a pointer tells a render nothing
about when its contents changed, so the component keeps the frame it first
derived and sits one store mutation behind until some unrelated prop or signal
moves. Nothing errors and every child is correct — the parent simply never
re-runs, which is why the child cannot recompute the active tab or the badge
for itself.

**Right** — `use_store()` for any component that RENDERS store state. It is the
same pump with `revision().read()` added, and that read is the only thing that
subscribes the scope to a mutation. Keep `use_pump()` for the components that
only need a handle: an event handler that dispatches, a provider that feeds an
effect, or a rig an async flow drives.

**Guard** — `crates/roost-web/tests/deck_store_subscription.rs` —
`a_deck_tab_click_reaches_the_deck_without_a_prop_changing` mounts the real
`TerminalDeck` over a real pump and asserts a `DeckIntent::SelectTab` reaches
the route the deck asks for although no prop changed, and
`only_the_subscribed_reader_repaints_when_the_store_moves` pins that a
`use_store` reader repaints on a mutation while a `use_pump` reader beside it
does not.

### The notification dock's bottom edge lands BELOW the compact composer's top edge

**Symptom** — "a toast covers the phone's chat input" / "the notification dock sits on top of the composer
instead of above it" / `getByTestId('notification-dock')`'s `bottom` is greater than
`getBoundingClientRect().top` of `.term-chat__dock[data-placement="viewport"]`, while the dock's own `width` is
correctly `390 - 24` and the composer is 96px tall. `--roost-notify-dock-lift` reads
`var(--term-chat-dock-offset)` — the INACTIVE compact branch — and the editor region still carries
`data-keyboard-shift`, both of which say the same thing: the shell believes no composer is mounted. Nothing
errors and every rect is an exact integer (12 / 96 / 832); the two surfaces are not fighting, they are both
reporting one shared value that is wrong.

**Wrong** — reconcile the two rects, or add slack to `notification_dock_lift`. The dock's `bottom` and the
shell's reserve are both computed from ONE published `ComposerGeometry`, so they cannot disagree with each
other; they can only both be wrong together, and slack in the lift hides it until the dock grows. Equally
wrong: hunting the CSS. `notification_dock_lift.rs`'s inactive branch is correct — it is what the shell should
be reserving when no composer is mounted.

**Right** — **a value whose `Drop` means "the dock unmounted" cannot be held by value in `use_hook`.** Dioxus
hands every render its own COPY of the hook it stores, and that copy dies when the render body returns, so
`Drop for ComposerClaim` fired on the render's copy rather than on the unmount: the slot went straight back to
`{active: false, height_px: 0}` while a 96px viewport composer was on screen. The release belongs on a
REFERENCE (`Rc<ClaimRelease>`, released when the last handle goes), and the hold belongs on the dock's
VISIBILITY rather than its mount — `ComposerSlot::set_on_screen` — because the drawer covers a dock without
unmounting it. This is NOT the entry above, "a component that renders store state through `use_pump` never
repaints": that one is a reader that never subscribes, and the fix is to add a revision read. This one had a
subscription and still lost its value, because the value itself was released under it. Do not merge them.

**Guard** — `crates/roost-web/tests/composer_slot_claim.rs` —
`a_dock_kept_in_a_hook_holds_the_slot` mounts a component that claims from `use_hook` and stays mounted, then
asserts `published_geometry().active`; `a_dock_that_unmounts_releases_the_slot` and
`a_dock_that_leaves_and_comes_back_keeps_its_hold` keep the fix from being "never release". The three fail
together against the by-value release.

### The mobile chat input is still mounted with the drawer open

**Symptom** — "the keyboard opens over the sidebar" / "I tap the folder drawer and the chat input is still
there" / `getByTestId('mobile-chat-input')` has count 1 while
`getByTestId('sidebar-drawer')` has `data-open="true"`. Nothing is stale on screen — the drawer really is open
and the composer really is mounted underneath it — so a page snapshot looks correct and only the count is
wrong. Any body-portaled surface with a drawer or overlay exclusion shows this at once.

**Wrong** — re-check the mount predicate. `mounts_viewport_composer` is correct, and so is the port of v2's
condition (v2's `CellTerminal.tsx:297-303`): `inLayout && focused && isCompact()
&& !uiStore.sidebarOpen && surfaceVisible` is spelled the same in both trees. The defect is not the predicate,
it is the value fed to it. In v2 the flag is read inside Solid's `<Show>`, so it re-evaluates when the drawer
opens and the subtree tears down. `CellTerminal` reads `core.borrow().store().ui.sidebar_open` through
`pump.core()` with NO subscription, and its only subscription is a `use_memo` over `PaneStoreView { pending,
title, offline_sibling }` — three fields the drawer does not move. The memo therefore produces a
`PartialEq`-equal value, Dioxus correctly does not notify, the pane never re-renders, and `drawer_open` stays
captured as the `false` it was when the composer mounted.

**Right** — **the scope of the subscription is the size of the value it renders, not the size of the
component.** `use_store()` (or a memo over `revision().read()`) for anything that renders store state,
because a `RefCell` behind a pointer tells a render nothing about when its contents changed. A body-portaled
surface has the sharper obligation: the decision that covers it lives in a store its host does not
re-render for, so the surface asks for itself on the revision — `composer_gate::use_drawer_open`, with the
same answer driving whether the dock renders and whether the shell reserves for it. This is the SAME invariant
as the `use_pump` entry above and it is NOT the same bug: that one is a MISSING read, this one is a read
taken through a memo whose value does not change, so the subscription exists and fires and resolves to
"nothing moved". Same lesson, different mechanism, different guard; do not merge them.

**Guard** — `crates/roost-web/tests/pane_drawer_mount.rs` mounts the real `CellTerminal` over a real
`Pump`/`ClientCore` and asserts the pane's dock is gone with the drawer open.
`crates/roost-web/tests/composer_slot_claim.rs` —
`the_drawer_covers_the_portaled_dock_and_not_the_pane_dock` pins the placement predicate natively.

---

## Sidebar, theming and design tokens

### An undefined token falls back to a hardcoded color

**Symptom** — "color shows as pitch black against new palette"

**Wrong** — `background: var(--bg-app, #111)` with `--bg-app` undefined → falls back to `#111`.

**Right** — every fallback must reference a defined token, OR the var must be declared in
`crates/roost-web/assets/styles/theme-vars.css`.

**Guard** — `cargo xtask lint`'s design raw-value ratchet (`xtask/src/design_raw.rs`) fails a new
`var(--x, #hex)` fallback, because the fallback is a raw hex. An undeclared token with no fallback has no
guard: none — the v2 guard `scripts/lint-roost.ts` (undeclared-`var()` rule) left with the TypeScript tree.

### Selected state derived from children instead of the URL

**Symptom** — "selected state lights everything coral"

**Wrong** — `data-selected={sessions().length > 0 ? "focused" : ""}`.

**Right** — `data-selected={useLocation().pathname.startsWith("/w/" + id) ? "focused" : ""}`.

**Guard** — `crates/roost-web/tests/sidebar_logic.rs` — `a_row_is_selected_only_by_its_own_route`.

### A full-surface loading or error card reads as a failure, then "flips" to the real UI

**Symptom** — "I press the button and it shows an error, then it flips to the folder list". No
console error, no failed RPC: on loopback the card is invisible, and on a phone over a tailnet it is
the whole screen for over a second.

**Wrong** — a route-level `<Show>` that swaps the ENTIRE page between states, with an
`EmptyState icon="progress_activity"` as the loading arm. `progress_activity` has no spin rule in
v2's styles, so the "loading" arm is a static icon-plus-text card structurally identical
to the error arm beside it, and the wholesale swap to the loaded UI is the "flip". The same shape
turns every listing failure into an empty result: a `.catch` that only nulls the data renders
"Empty folder" for a directory that failed to read.

**Right** — mount the chrome once and switch only the content region. A surface owns an explicit
status (`loading | ready | error | offline`), loading paints skeleton rows shaped like the rows that
will replace them, and a failure keeps its own reader-facing copy plus a Retry that re-runs the
fetch. Denial replacements that a test pins by accessible name (here `browse-worker-unavailable`)
stay byte-identical and take the content region's place, never the page's.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A flex column with no width bound grows to min-content and clips every row

**Symptom** — on a phone, a surface's controls and text run off the right edge with no horizontal
scrollbar: a button label half-cut, an empty state's sentence clipped mid-word, `flex-wrap: wrap`
visibly not wrapping. `document.documentElement.scrollWidth > window.innerWidth`.

**Wrong** — relying on `flex-wrap: wrap` inside a `flex-direction: column` panel that has no
`max-inline-size`. The panel's own width resolves to the widest row's min-content, so the row it was
supposed to wrap always "fits" and never wraps; a native `<input>` (~20ch intrinsic) or a
non-shrinking label is enough to widen the whole page. Hiding the labels on compact
(`display: none`) papers over it and costs the phone the very affordances it needs.

**Right** — bound the panel (`max-inline-size: 100%` plus `min-inline-size: 0`) and let the
intrinsically-wide children shrink (`min-inline-size: 0` on the field wrapper AND on
`.roost-text-field__control`). Then wrapping happens, and every label can stay visible at every
width.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### An inline-size container query pins every flex item to its minimum width

**Symptom** — "the tabs don't fit properly": every tab in a strip sits at its floor width while the
strip still has hundreds of free pixels, and the control that follows the scroller ends up far right
of the last tab.

**Wrong** — `container-type: inline-size` on the flex item plus a flex BASIS for its resting width
(`flex: 0 1 var(--workbench-tab-width-max)`) inside a shrink-to-fit scroller (`flex: 0 1 auto`).
Inline-size containment zeroes the item's intrinsic contribution, so the scroller's content-based
base size resolves to the sum of the items' `min-inline-size` — not their flex bases. Every item is
at its floor from the second item on, and every container query written for the floor fires at all
widths.

**Right** — give the contained item a DEFINITE outer size (`inline-size:
var(--workbench-tab-width-max)` beside `min-inline-size` / `max-inline-size`). The flex base size
then comes from the width property, the scroller's max-content resolves to n×max, and items land
uniformly at `clamp(min, rail / n, max)`.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A sticky control inside a scroller is painted over by the content it must clear

**Symptom** — "existing tabs paint over the + button": a scrolled inactive tab merges into the
trailing button, the active tab is sliced at the button's left edge, and a dragged tab floats over it.

**Wrong** — parking the trailing control inside the scrolling rail as `position: sticky;
inset-inline-end: 0; z-index: 1` over an opaque background, and reserving its width with
`padding-inline-end` on the scroll content. Its z-index loses to any item that raises its own (a drag
lifts the grabbed tab), and the reserved padding inflates `scrollWidth`, a signal the chevron must
never read: a rail wide enough for every tab still reports a scroll extent past its own client width
once a lifted tab or the reorder spring translates one, so the chevron parks on a strip where nothing
is out of reach.

**Right** — make the control a flex SIBLING after the scroller, and read the chevron's overflow off
CLIPPED TABS: a tab's own box against the rail's box (`clips_rail`,
`crates/roost-web/src/components/deck/deck_dom.rs`). The scroller clips its content at its own edge,
so no scroll position, drag, or close animation can reach the control, and only a tab's box answers
the question the chevron exists for — whether a tab is out of view.

**Guard** — `crates/roost-web/src/components/deck/deck_dom.rs`: `clips_rail` tests
`six_tabs_at_the_floor_are_clipped_exactly_where_the_row_leaves_the_rail` (the 800px packed strip)
and `six_tabs_that_fit_are_not_clipped_on_a_rail_whose_scroll_extent_overruns` (the 1024px strip).

---

## Terminal history, rendering and scroll ownership

### Remounting the terminal on navigation destroys the session

**Symptom** — "terminal disconnects on nav / lost scrollback"

**Wrong** — `<Show when={activeSession()}>{(s) => <CellTerminal .../>}</Show>` (remount per nav).

**Right** — every open session stays mounted in the deck (`crates/roost-web/src/components/deck/terminal_deck.rs`)
and is shown or hidden by `visibility: visible↔hidden`. The deck host stays mounted for every MainPane screen so
a `/file` or `/search` visit never tears it down.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### Torn seam between retained history and the live stream

**Symptom** — "scrollback seam torn — duplicated tail, missing unchanged cells,
or two different terminals after a tab switch/reconnect"

**Wrong** — infer continuity from mount state, a remembered applied sequence, a
zero-byte reveal witness, or a bounded mount-gap buffer. Those mechanisms have
different lifetimes and can accept a delta after the component that owned its
baseline disappeared.

**Right** — three explicit replicas and full-before-delta sequencing. Worker
frames carry UUID `stream_id`, opaque `gridEpoch`, monotonic `seq`, and exact
`base_seq`. `TerminalScreenHub` accepts a delta only against its complete
coordinator replica; the client core applies the same rule to its
per-session browser replica. A gap latches one snapshot request. Chunked fulls
install atomically only after every viewport row occurs exactly once. Renderer
mount state is not part of the continuity proof.

**Guard** — `crates/roost-protocol/tests/cell_grid_chunk_planning.rs` —
`a_forced_small_full_reassembles_to_the_original_frame` and
`a_frame_over_one_mib_is_split_on_whole_row_boundaries`
(the protocol-side chunk planner);
`crates/roost-coord/tests/terminal_screen_hub.rs` —
`a_stale_stream_is_ignored_and_a_broken_delta_run_latches_one_repair` and
`the_old_baseline_stays_served_until_a_replacement_assembles_completely`;
`crates/roost-client-core/tests/terminal_full_before_delta.rs` (the browser replica).

### Inferred scrolled-off rows freeze a stale repaint generation into history

**Symptom** — "the same TUI block repeats on consecutive rows and every copy is
a different generation — a different spinner frame per row, `5m` on one status
bar and `3m` on the next; duplicated card headers stacked above the live pane of
an inline (main-screen) agent TUI"

**Wrong** — infer WHICH rows left the viewport from `scrollbackTotal` growth and
paint the previously held viewport head into
`[previous.scrollbackTotal, frame.scrollbackTotal)` (`transitionedViewportRows`).
Equal epoch, cols, rows and alt-screen are admission facts about the GRID, never
proof about row CONTENT, so a TUI that repaints a block in place (cursor-up plus
rewrite) before the grid scrolls gets whichever generation the browser happened
to hold frozen into the next absolute index — one stale spinner glyph, one stale
elapsed time, per checkpoint, and checkpoints are frequent for a chatty pane.
Nothing ever contradicts the guess: canonical frames are normalized
viewport-only (the protocol crate's cell-grid normalisation),
so no authoritative history disagrees, and the renderer's page splice deliberately tolerates
a refused re-insert, so demand backfill never repairs those rows either.

**Right** — **painted history content is only ever the worker's own rows.** A
canonical viewport-only checkpoint reserves
`[previous.scrollbackTotal, frame.scrollbackTotal)` as an UNPAINTED gap
(the renderer's scrollback-gap reservation in `crates/roost-web-terminal`, which already
holds that interval's exact pixel height) and lets the epoch-addressed,
worker-authoritative `SessionsGetScrollbackCells` backfill fill it on demand;
only a frame's own `scrollbackRows` / `scrollbackAppend` are ever painted.
Non-contiguous painted history is a first-class state
(the client core's missing-history-range set, `crates/roost-client-core/tests/backfill_history_ranges.rs`), so a
reserved gap needs no inference to stand in for it. A matching history/head
boundary identifies a content-PROVED shift candidate, but global viewport reuse
is permitted only when `deltaViewportShift`
(`crates/roost-protocol`'s viewport-shift diff) receives the complete final viewport.
`applyDelta` and `foldCellDeltaBatch` then reuse that global shift; sparse
deltas patch only their worker-authored final coordinates and retain omitted
held rows, so a fixed footer cannot receive an older status generation.

**Guard** — `crates/roost-protocol/tests/cell_delta_batch.rs` —
`a_sparse_partial_region_batch_preserves_the_untouched_footer`;
`crates/roost-protocol/tests/cell_delta_admission.rs` —
`a_shift_is_only_reused_when_the_boundary_row_actually_matched`, and
`a_link_difference_defeats_the_shift`;
`crates/roost-web-terminal/tests/render_reconcile_diff.rs` —
`a_partial_region_scroll_retains_the_fixed_panel_and_worker_history`,
`a_batched_partial_region_scroll_retains_the_fixed_panel_and_latest_status`;
`crates/roost-web-terminal/tests/render_history_checkpoint.rs` —
`a_checkpoint_leaves_the_transitioned_rows_unpainted_for_authoritative_backfill`.

### Transient chrome resizes the PTY and an inline TUI duplicates rows into history

**Symptom** — "the same 2-row (or N-row) block repeats down the scrollback while an
agent streams / a spinner or status line gets pushed up instead of repainting /
worse on mobile"; the duplicates ARE in the worker's own history
(`roost api cells <sid>`), and the worker's `terminal-view` `stream_desired` log
shows `rows` stepping (46→44→42…) at a fixed `cols` while the user types.

**Wrong** — hunt the renderer, the emitter, or history backfill without first
checking core parity. `crates/roost-term/tests/terminal_core_vectors.rs` runs the
xterm-oracle vectors in `protocol/conformance/terminal-core/`; the real-stack smoke suite then
proves what the worker retained. In the height-step incident the rows are real:
every PTY height change makes an inline TUI repaint, and a TUI that repaints IN
PLACE (omp latches this for the life of the process after a height-only change
around an alt-screen overlay; tmux-style panes do it always) leaves the rows a
shrink pushed into history duplicated there. Equally wrong: debouncing or
hysteresis on the published geometry — each surviving resize still duplicates.

**Right** — **transient chrome never changes the terminal grid.** The desktop pane
composer reserves only the pill's one-line resting height
(`crates/roost-web/src/components/terminal_chrome/pane_geometry.rs`); a longer draft or a
status overflows upward and `CellTerminal` translates the display instead of
shrinking it. The compact shell (`crates/roost-web/src/components/layout/app_shell.rs`) reserves the composer's resting
row on every terminal route whether or not the composer is mounted (it unmounts
under the drawer) and handles the soft keyboard only by translation —
`--term-chat-dock-rest-offset` excludes `--kb-offset`. Only a real pane/window
resize (or the explicit `keyboardResize` preference) may resize the PTY.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A fast in-place row rewrite duplicates rows into history

**Symptom** — "a spinner/status row rewritten rapidly stacks copies into
scrollback in Roost but not in other terminals; the copies are in `roost api
cells`".

**Wrong** — fix the renderer or fold, which only paint worker-authored rows, or
blame the application without first proving terminal-core parity.

**Right** — the terminal core must match xterm deferred-wrap and margin semantics:
LF clears pending wrap at `cols - 1`; CUU/CUD (and therefore CPL/CNL) clamp to
DECSTBM; VPR remains screen-clamped; RI clears pending wrap. Prove every core
change with an xterm-oracle vector under `protocol/conformance/terminal-core/`.

**Guard** — `crates/roost-term/tests/terminal_core_vectors.rs` — `every_terminal_core_vector_matches`
(`deferred-wrap.json`, `cursor-margins-clamp.json`).

### Alt-screen wallpaper of stale text after a worker restart

**Symptom** — "after worker restart an alternate-screen session shows wallpaper of stale text + overlapping/parallel lines"

**Wrong** — `resume()` rebuilds an empty wtermCore + records alternate-screen state, but the snapshot taken from
that rebuilt core reads `core.usingAltScreen()` (false on an empty core) → the fresh snapshot reports
main-screen → live alt redraws land in main-screen.

**Right** — **prime the rebuilt core's alt state** on resume (`crates/roost-worker/src/session/resume_core.rs`) whenever
the retained session state says it was using the alternate screen: `wtermCore.writeRaw(ALT_ENTER_SEQS[0])` after
the core is created so `core.usingAltScreen()` matches the retained state. NOT a forced SIGWINCH (TUIs repaint
alt but do not necessarily re-send `?1049h`).

**Guard** — none — the v2 guard `apps/worker/tests/session/session-manager-altmode.test.ts` left with the TypeScript tree.

### History gone after a worker restart because the keeper retained none

**Symptom** — "history GONE after worker restart + browser refresh; pane freezes / seq-epoch reset / 'new browser fixes it'"

**Wrong** — `resume()` rebuilds `scrollback:new Uint8Array(0), head_seq:0` because the keeper retained NO
per-channel history → the SPA's persisted lastSeq goes stale-high → seq-epoch reset, history unrecoverable.

**Right** — **the keeper retains a per-channel output ring and head sequence**
(`crates/roost-keeper/src/output_ring.rs`, advanced in the same callback that broadcasts so it matches
the worker count); `GetHistory`/`GetHistoryResp` are represented by the authenticated
`KeeperContractV1` feature sets. Boot adopts a protocol-compatible survivor, but an
incompatible survivor blocks replacement while coordinator sessions or keeper bindings
remain live. Resume re-reads via the keeper pool's history call and seeds
`scrollback`+`head_seq`; only a proven-empty incompatible keeper may be replaced.

**Guard** — `crates/roost-worker/tests/keeper_survivor_adoption.rs` —
`a_restarted_worker_reattaches_at_the_history_boundary`;
`crates/roost-worker/tests/boot_adoption_gate.rs` —
`a_restarted_worker_adopts_its_survivor_from_the_keepers_history`.

### No scrollbar in the terminal — the container CSS, not the core

**Symptom** — "no scroll bar / mouse wheel does nothing in terminal / can't scroll up to see history"

**Wrong** — switch to alternative terminal cores / upstream the core / patch the WASM "because
getScrollbackCount returned 0 in my synthetic test".

**Right** — **`.wterm { overflow-y: auto; overflow-x: hidden; }` in `crates/roost-web/assets/styles/sidebar.css`.** The
core DOES populate scrollback and the renderer DOES emit scrollback row DOM; the only thing missing was the
container CSS that lets those rows be scrolled. A synthetic test reporting zero scrollback usually means the
renderer hasn't painted yet (rAF does not fire in background tabs) — force a render before checking. DO NOT
switch terminal cores; the bug is one CSS rule.

**Guard** — none — the v2 guard `scripts/lint-roost.ts` (`.wterm` overflow rule) left with the TypeScript tree.

### The whole screen rubber-bands when a touch drag runs out of scroll

**Symptom** — "on mobile, dragging at the bottom drags/bounces the whole screen",
"the page gets pushed while I scroll the terminal", "pull-to-refresh fires inside
the app".

**Wrong** — a JS rubber-band, a `touchmove` `preventDefault()` race, or
per-component scroll locks. A deleted component-local handler was exactly that:
once the browser has started a scroll, later `preventDefault()` is ignored.

**Right** — **declare the policy once in `crates/roost-web/index.html`'s base style:
`* { overscroll-behavior-y: none; }` plus `html, body { overflow: hidden; }`.**
The first kills chaining and the local elastic edge for every scroller
(`.wterm` included); the second denies the document a scroll range at all
(`height: 100%` resolves against the large viewport while `.workbench-shell` is
sized in `svh`). For a gesture the terminal application owns, `touch-action`
flips to `none` on the pane display (`CellTerminal.tsx`, keyed on
`mouseGesturesForwarded`) so the browser never starts a pan to begin with.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A list refuses to scroll while the cursor sits on a row's clipped label

**Symptom** — "the agents list only scrolls from some parts of a row",
"the wheel does nothing over the row title but works 20px lower", a wheel event
that reaches the row (`defaultPrevented` false) with no `scroll` event on the
list.

**Wrong** — blame scroll latching, relax the gate that wheels at a panel's
centre, or narrow `* { overscroll-behavior-y: none; }` in
`crates/roost-web/index.html` (the entry above owns that policy, and its smoke
specs pin it). Nothing about the scroller is broken: `scrollTop` writes still
land, and the same wheel scrolls from a neighbouring pixel.

**Right** — **a box that only clips text must not swallow the gesture.**
`overflow: hidden` makes it a scroll container, the global policy then denies
that container scroll chaining, and a wheel landing on the text is consumed by
a container with no scroll range instead of reaching the list. Each text clip a
pointer can land on inside a scroller opts chaining back in with
`overscroll-behavior-y: auto` (`.md-list-row__headline` in
`crates/roost-web/assets/components/Settings/md/tokens.css`;
`.workbench-sidebar-agents__group-name`, `__group-server` and
`.agent-status-rollup` in `crates/roost-web/assets/styles/workbench-sidebar.css`). It has
no scroll range of its own, so chaining there can only reach the list, and the
list's own `none` still stops the chain from reaching the document. Rows that
cover their whole area with a hit target (`.df-row__primary` in the folders
list) never showed the defect, which is why it surfaced only in the agents
list.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### Scrollback mangles or drifts with no user action

**Symptom** — "terminal history changes after another viewer joins, leaves, or
resizes; unchanged TUI cells disappear"

**Wrong** — let browser mounts and worker claim reapers independently choose
geometry, or rebuild the emulator from a bounded raw-byte ring after every
resize. A rebuild cannot recover bytes already evicted and silently converts
unchanged cells into blanks.

**Right** — `TerminalViewHub` is the sole SCD owner and independently minimizes
active rows and columns. The worker applies that one geometry synchronously to
the existing wterm core at the keeper's ordered `ResizeAck` boundary: earlier
bytes parse at the old size, `wtermCore.resize` runs before the callback
returns, and later bytes parse at the new size. The resize forces a complete
new-stream baseline but never reconstructs ordinary live state. Worker-history
replay is reserved for genuine process adoption when no in-memory core exists.

**Guard** — `crates/roost-coord/tests/terminal_view_geometry.rs` —
`a_viewers_departure_recomputes_the_effective_geometry`;
`crates/roost-worker/tests/terminal_stream_state.rs` —
`shrink_and_grow_resize_the_same_core_at_the_keeper_boundary`.

### A session stays clipped to a viewer that is no longer looking

**Symptom** — "terminal is stuck narrow / clipped / wrong size after another
device disconnected, went to sleep or was backgrounded; the grid only grows
back once the other tab reloads or ~15 seconds pass"

**Wrong** — let one record set answer two different questions: who is still a
member, and whose dimensions bind the PTY. `terminalViewGeometries` filtered
nothing — parked and lease-expired-unswept records still shrank the session —
while its sibling `broadcast()` filtered `parked`. Same records, two different
membership predicates, and the permissive one decided the PTY size. The second
mistake hid the first: a hand-rolled per-axis minimum sat next to the shared
primitive, so grepping `minimumTerminalGeometry` made the policy look pinned
while the decider used its own copy.

**Right** — **ONE predicate decides geometry membership and ONE primitive
computes the minimum.** `minimumTerminalGeometry` (`@roost/protocol/viewport`) is
the only per-axis aggregation anywhere. A parked view keeps its lease,
membership and tombstone for reclaim, but stops constraining geometry once
`TERMINAL_VIEW_PARK_GRACE_MS` lapses — reclaim and geometry are different
questions. With zero live viewers the last effective geometry is HELD, never
re-minted, so a flapping link cannot become a stream re-mint storm. The
per-viewer inputs that produced the minimum are observable from outside the
process: `DiagSnapshot`'s `coord.sessions[<id>].viewers[]` carries
`{ fingerprint, viewId, cols, rows, parked, constrains }` — the smoke probe
surfaces the same array as `terminal_control.viewer_inputs` — and `constrains`
is true exactly for the records the live predicate admitted, so "who is
pinning this session?" is answerable without attaching a debugger.

**Guard** — `crates/roost-protocol/src/viewport.rs` —
`the_minimum_is_taken_on_each_axis_independently` and
`the_minimum_refuses_an_input_it_could_not_aggregate` (the single per-axis
aggregation primitive this entry is about);
`crates/roost-coord/tests/terminal_view_geometry.rs` —
`an_expired_lease_stops_pinning_the_pty`, `a_solo_viewers_geometry_is_held_across_a_link_blip`;
`crates/roost-worker/tests/terminal_view_owner.rs` —
`a_parked_viewer_stops_constraining_only_once_its_grace_lapses`;
`crates/roost-coord/tests/diag_snapshot_session_state.rs` —
`reports_every_viewer_input_and_the_minimum_over_the_constraining_ones`;
`crates/roost-coord/tests/workers_send.rs` — `a_respawn_uses_the_effective_geometry_of_its_viewers`.

### Attach/reveal cost proportional to scrollback depth

**Symptom** — "attach/tab-switch/resize slow proportional to scrollback depth / long sessions stall seconds on pull-in while fresh ones are instant"

**Wrong** — ship the ENTIRE retained scrollback (≤10k rows) in every full cell frame — O(history) snapshot work
inside the claim, one MB-scale proto blob head-of-line-blocking the Sync stream, O(history) decode+DOM on the
SPA before first paint; or "fix" it by racing/timeouting the history away (trading history for latency is
forbidden).

**Right** — **history is PULLED on demand and never shipped wholesale.**
The renderer's backfill (`crates/roost-web-terminal/src/backfill.rs`) pulls ranges in chunks via `SessionsGetScrollbackCells` (coord relay →
the worker's scrollback page reader in `crates/roost-worker/src/browser_commands/scrollback_page.rs`, serving
rows from `crates/roost-worker/src/scrollback_read.rs`) and the renderer
(`crates/roost-web-terminal/src/cell_renderer.rs`) splices above the reader. At a literal bottom the renderer pins the new
bottom; otherwise it leaves `scrollTop` untouched. History ALWAYS arrives — only its timing is lazy. The
intermediate form (a fixed 250-row tail in every full frame plus a `mergeFullFrame` tail merge) is RETIRED: full
frames are now viewport-only, see the epoch-addressed entry below.

**Guard** — `crates/roost-worker/tests/scrollback_page_window.rs` — `a_window_inside_the_grid_is_served_whole`;
`crates/roost-web-terminal/tests/render_append_frames.rs` —
`a_viewport_only_full_reserves_depth_and_explicit_pages_fill_the_seam`.

### A demand page is only issued once the rows are already blank

**Symptom** — "scrolling up into history is slow / takes a while to load while I scroll / the rows are empty and it looks like nothing is loading"

**Wrong** — page only what the reader can ALREADY see: demand the missing interval the CURRENT visible window
exposes (`missingScrollbackRangeAtScroll` with no read-ahead), size the page from the focus row FORWARD
(`end = min(gap.end, focus + PAGE)` — clamped to the painted edge, that is the ~35-row sliver the step exposed,
so page size never even bites), and treat every scroll event as a new demand that `generation++` orphans the
in-flight page for. Measured on a hermetic stack with a 5000-row history: SIX one-viewport wheel steps cost SIX
serialized demand round trips — one per screen, each one in the reader's critical path, and a remote reader
pays a full RTT for every screen. Equally wrong: "fix" it by shipping history wholesale again (the entry above),
or by widening the page while leaving the trigger and the anchor alone — a bigger page clamped to the painted
edge is still one sliver per screen.

**Right** — **pre-pay the rows the reader is scrolling TOWARD, one wave at a time.** Three rules in
`crates/roost-web-terminal/src/backfill.rs`, and they only work together: (1) the trigger window is widened upward
by `BACKFILL_AHEAD_ROWS`, so a demand is raised before the rows are visible — the bottom-most missing interval
still wins, which is what keeps a visible gap ahead of a read-ahead gap; (2) a `scroll` page is anchored at the
NEWEST missing row of that interval and extends `BACKFILL_FETCH_ROWS` (250 = one `SB_BLOCK`) OLDER, so one
round trip covers the blank sliver plus the next several screens, while a `find` page keeps advancing forward
from its match (a match needs the context newer than itself); (3) exactly ONE wave is in flight — a scroll
raised mid-wave is coalesced instead of orphaning a page the worker already read and the wire already carried,
and every settle RE-DERIVES the demand from live scroll state. That re-derive is load-bearing: `splicePage`
reports false on benign paths, so a pager that only re-armed on success would leave the gap blank until the
next scroll event — which is the symptom. An unchanged derivation relaunches a bounded number of times
(`BACKFILL_IDENTICAL_RETRIES`) and then falls back to the `BACKFILL_RETRY_MS` cadence instead of waiting for a
reader who may never scroll again: one wave per interval cannot hot-loop, and a reader parked on rows nothing
has painted keeps getting waves. A page also lands in exactly ONE placeholder, so `scrollDemandBounds` /
`findDemandBounds` pick the side of the painted head base (`backfillAnchor().sbBase`) that holds the row the
wave owes — a page spanning the head spacer and the gap above it is refused by `_insertPageIntoPlaceholder`,
and a reader dragged to the top of history is exactly where that page shape arises.
Same measurement after: ONE demand wave, then every step until the pre-paid lead is consumed crossing
already-painted rows at zero RPCs, and the next one re-arming the pager exactly where the band predicts. The
page geometry itself is pure and lives apart in `crates/roost-client-core/src/terminal/history_backfill.rs`; each new pager
state (`scrollback.demand_coalesced`, `demand_rearmed`, `demand_retry_deferred`, `demand_retry_woke`) emits one
`diag()` line, the deferred pair naming the state that used to leave a visible gap unpainted. Unpainted
placeholders also stop reading as empty — `.cell-grid .cell-sb-gap` / `.cell-sb-spacer` in
`crates/roost-web/assets/styles/sidebar.css` paint a row-pitch skeleton (paint-only; those elements' inline pixel heights
are what every scroll position is derived from, so never give them geometry). The head spacer drops that
texture only while it lies ENTIRELY below a proven retention floor (`setHistoryFloor` →
`data-history-floor`, re-derived in `_syncSpacer`): those rows are gone, not loading, and a pending sheet over
them would read as a load that never ends — but a floor proven for an interior gap must not suppress head rows
that are still pageable.

**Guard** — `crates/roost-client-core/tests/history_backfill.rs` — the page never collapses to the sliver the window exposed,
whatever the interval's shape (`a_steady_scroll_up_page_ends_at_the_painted_edge_and_extends_a_page_older`,
`a_page_never_spans_the_painted_base_the_readers_own_side_of_it_wins`);
`crates/roost-web-terminal/tests/backfill_demand_waves.rs` —
`a_wheel_step_pre_pays_the_rows_above_the_viewport_and_the_next_step_is_free`,
`scrolls_during_a_wave_add_no_request_one_coalesce_line_and_one_demand_after`,
`a_page_that_cannot_splice_retries_bounded_and_stays_armed_for_the_reader`,
`the_readers_own_rows_paint_when_one_page_would_span_the_painted_base`,
`a_spent_budget_keeps_re_deriving_on_the_retry_cadence_one_wave_per_interval`,
`suspend_and_dispose_cancel_the_deferred_rearm`.

### Scroll position lurches — many writers of scrollTop

**Symptom** — "terminal scrollback jumps around / view lurches while scrolling up / lands mid-history after a tab switch / drifts off the bottom after vim/less/claude exits"

**Wrong** — row-space or pixel scroll ownership: intent/anchor state, distance compensation, scroll-event
classification, resize/reveal correction, or a jump-to-bottom control.

**Right** — **one pre-mutation capture plus ONE conditional writer.** `CellGridRenderer`
(`crates/roost-web-terminal/src/cell_renderer.rs`) captures `_atBottomOrOwnedPlacement()` before a painted-height
mutation; only `_pinToBottom(shouldPin)` may assign `scrollTop`, and only when that captured value was
true. The capture is the FOLLOW BAND (`followsScrollBottom`, two rows of slack — see the follow-band
entry below), never a widened `atBottom()`: `atBottom()` itself stays exact and is what the clamp and
settle paths keep asking. Non-bottom mutations never write position. The mutable append tail is
`overflow-anchor:none` so Chromium does not follow it when the reader is one pixel above bottom;
completed blocks are anchors, and the tail is restored before a backfill prepend so native anchoring
preserves the reader's row. Never restore intent state, add reveal correction, or a jump-to-bottom
control. This single-writer invariant is about POSITION and
presumes the scroll SPACE is truthful — the spacer entry below is what makes it so.

**Guard** — `crates/roost-web-terminal/tests/render_reader_park.rs` —
`a_non_bottom_history_page_performs_no_application_scroll_write`;
`crates/roost-web-terminal/tests/render_held_window.rs` —
`only_the_mutable_tail_is_excluded_from_browser_anchoring`;
`crates/roost-web-terminal/tests/render_reader_live.rs` —
`unchanged_and_fully_clamped_pins_leave_no_stale_scroll_ownership`,
`a_coalesced_pin_retargets_once_then_the_next_native_scroll_reads`.

### A parked pane paints at a lying box size

**Symptom** — "tab switch shows stale terminal content / a returned-to pane sits above the live bottom and never follows output again / bottom-follow works foreground but dies after a park"

**Wrong** — latch the bottom in intent state, correct scroll at reveal, add an `atBottom()` tolerance (the
CLAMP predicate stays exact; slack lives in the separate follow-band predicate, see the follow-band entry
below), or defer
`_pinToBottom` to a rAF — all forbidden by the entry above; equally wrong: leave a parked pane painting at a
DIFFERENT box size (the old fixed 800×600 park) so its scroll maximum moves under it.

**Right** — **a pane that keeps painting off-screen must have TRUTHFUL geometry, not a corrected scroll
position.** Three invariants, all measured live: (1) the deck parks a pane at its own leaf's rect
(`crates/roost-web/src/components/deck/terminal_deck_geometry.rs`) so `clientHeight` is identical parked vs
revealed; (2) block placeholders are a BARE length, never `contain-intrinsic-size: auto <len>` — `auto` makes
the browser reuse a block's LAST RENDERED size, so a block that grows while skipped understates `scrollHeight`
until it materializes; (3) the OPEN tail block opts out of `content-visibility` until it seals — a skipped
subtree's intrinsic size is re-evaluated at rendering-lifecycle time, not on append, so appending into a locked
tail leaves `scrollHeight` stale and the pre-mutation bottom check reads a bottom that no longer exists. Sealed
blocks stay skipped, so deep-history layout stays O(blocks). Measured: a 250-row block remembered at 29 rows
reported 487.11px instead of 4199.22px; revealing it grew `scrollHeight` by exactly that 3712px.

**Guard** — `crates/roost-web-terminal/tests/block_placeholder.rs` —
`the_placeholder_is_a_bare_length_never_the_self_correcting_auto_form`;
`crates/roost-web-terminal/tests/render_held_window.rs` —
`only_the_open_tail_block_opts_out_of_content_visibility_and_sealing_restores_it`;
`crates/roost-web-terminal/tests/render_geometry.rs` —
`an_at_bottom_reader_follows_a_box_grow_onto_the_new_bottom`,
`a_live_old_bottom_anchor_follows_a_box_shrink_with_exactly_one_pin`;
`crates/roost-web/tests/deck_geometry.rs` — `a_parked_terminal_stays_laid_out_at_its_reveal_size`.

### A box grow under a parked reader removes the last scroll event

**Symptom** — "terminal died after the window/pane/composer changed size and never streamed again / scrolling
back to the bottom does not restart it / only a reload fixes it", with `at_bottom` already TRUE and
`reconcile_block_reason=reader_pending_frame` while the canonical seq keeps climbing.

**Wrong** — resume a parked reader from `ResizeObserver` only when `_readerReason === "native_scroll"`. Real
wheel and touch gestures park as `"wheel"` / `"touch"` (`crates/roost-web-terminal/src/mouse_forward.rs`), so that
gate was dead for every real gesture. Equally wrong here: an `atBottom()` tolerance or any reveal/resize scroll
correction — the geometry was measured INTEGRAL in 184 real layouts at four device-pixel ratios (the true clamp
equals `scrollHeight - clientHeight` exactly), so the clamp predicate was never the defect (what a reader near
the tail is ALLOWED to do is a separate policy — see the follow-band entry below). Equally wrong: gating the
bottom-clamp settle on the `native_scroll` reason alone. The scroll handler records `native_scroll` for a real
movement and the mouse forwarder then UPGRADES that park to the gesture's own `wheel`/`touch` reason,
so the settle armed for the next frame no longer matches its own park; if layout clamps that park onto the
exact bottom, no further scroll event exists and nothing resumes it. Also wrong: letting a paint hold swallow
the repair — `noteBoxResize` advances `_lastBoxH` before it returns, so the ResizeObserver cannot retry, and
`_resumeLive` refuses under a hold, so a wheel park plus a link hover plus a zero-range grow consumed the
pane's only resume while the later hold release refused too, because release resumed a `selection` park only.

**Right** — **a park must be exitable by an event the pane can still deliver.** A parked reader freezes the
DOM, so a grow past the frozen content leaves `scrollHeight === clientHeight`: the box can never fire another
scroll event, `handleScroll()`'s bottom resume (`crates/roost-web-terminal/src/cell_renderer.rs`) is unreachable, and
`noteBoxResize()` is the only observer left — it also consumes `_lastBoxH` before every early return, so a
refusal is permanent. `noteBoxResize()` therefore resumes when the reader sat at the old box's bottom AND the
park is position-only (`isPositionOnlyReaderReason` — `native_scroll`/`wheel`/`touch`), and resumes ANY park,
`find` included, when the post-resize box has no scroll range left. An off-bottom wheel/touch park with range
remaining keeps its park (a real wheel still recovers it). Measured wedge: `scrollTop 0 / scrollHeight 748 /
clientHeight 748`, DOM pinned at the old epoch seq 28 while canonical reached 52 on a new epoch; zero scroll
events across ten wheel bursts, a `scrollTop = scrollHeight` assignment and a click.
The settle is keyed to the same position-only class as the resize resume, and still demands
`readerIntent === "reading"`, `!holding` and `atBottom()` exactly — the follow band never widens this
rAF settle — so no off-bottom or held reader is
un-parked.
A hold release resumes the selection park the hold itself created, any park once the box has no scroll range,
and a position-only park that is inside the follow band (the hold swallowed the scroll event that proved the
return, and no further event follows), explicitly, so a `find` park is released by the no-range case too; an
off-band park that still has range keeps its interval, because the exact-bottom scroll, the next frame's
bottom-clamp settle and the scroll-idle band settle own that case.

**Guard** — `crates/roost-web-terminal/tests/render_geometry.rs` —
`a_wheel_parked_reader_resumes_when_a_box_grow_leaves_no_scroll_range` and its siblings;
`crates/roost-web-terminal/tests/render_scroll_settle.rs` —
`a_wheel_park_clamped_to_the_bottom_settles_without_a_second_scroll_event`;
`crates/roost-web-terminal/tests/render_append_holds.rs` —
`a_hold_release_resumes_a_wheel_park_whose_box_lost_its_scroll_range`, with
`a_hold_release_leaves_a_find_park_that_can_still_reach_its_anchor` as the refusal control.

### The terminal stops streaming after the smallest scroll

**Symptom** — "terminal stops streaming after the smallest scroll / the blue live dot turns amber when I barely
move / a trackpad micro-tick freezes the pane / scrolling all the way back down does not restart it"

**Wrong** — widen `atBottom()` itself: the clamp check and the rAF bottom-park settle must stay exact (the two
entries above). Equally wrong: resume synchronously from `handleScroll()` when merely NEAR the bottom — a
`scrollTop` write mid-gesture cancels the scroll animation Chromium is still running and eats the reader's own
gesture. Also wrong: a jump-to-bottom control, or latching intent state so a park "remembers" it wanted to be
live.

**Right** — **a named follow band gates the POLICY decisions; the exact predicates stay exact.**
`BOTTOM_FOLLOW_SLACK_ROWS` (2) and `followsScrollBottom` in
`crates/roost-web-terminal/src/presentation.rs` define one band around the clamp, and only four call sites
use it: `handleScroll()`'s park decision, the pin capture `_atBottomOrOwnedPlacement()`, the backfill
demand gate (`scrollbackBackfill.onUserScroll`, a band follower must not start paging history), and the
band settle. A reader inside the band is riding the tail, so the pane keeps painting and keeps pinning;
one wheel notch (~100px) is outside it and still parks. A park that comes to REST inside the band resumes
through `CellGridRenderer.settleFollowBand()`, armed `BOTTOM_FOLLOW_SETTLE_MS` (180ms) after the last
scroll event by the pane's own scroll listener (`crates/roost-web/src/components/terminal/cell_terminal.rs`) and
also by frame arrival (see the entry below), so the resume never runs mid-gesture. A hold release also
resumes a band-following
position-only park, because the hold swallowed the only scroll event that could. `at_bottom` in the
presentation snapshot keeps its exact meaning; `follows_bottom` is the band value beside it.

**Guard** — `crates/roost-web-terminal/tests/render_reader_live.rs` —
`a_live_reader_inside_the_follow_band_keeps_following_the_tail`;
`crates/roost-web-terminal/tests/render_scroll_settle.rs` —
`a_wheel_park_resting_inside_the_follow_band_resumes_on_the_settle`, with
`a_park_beyond_the_follow_band_survives_the_settle` and
`a_find_park_inside_the_follow_band_keeps_its_anchor_through_the_settle` as the refusal controls;
`crates/roost-web-terminal/tests/render_append_holds.rs` —
`a_hold_release_resumes_a_bottom_following_wheel_park_that_kept_its_range`.

### A wheel park created after the gesture's last scroll event never resumes

**Symptom** — "I am sitting near the bottom, output stops, the dot goes amber and stays there; typing is the
only thing that brings it back" — `reconcile_block_reason=reader_pending_frame` with `follows_bottom` TRUE,
`at_bottom` FALSE, `reader_reason` `wheel` or `touch`, and canonical climbing away from the DOM forever.

**Wrong** — arming the band settle from the pane's scroll listener ALONE. `enterReadingForNativeScroll`
(`crates/roost-web-terminal/src/mouse_forward.rs`) parks from a capture-phase, non-passive wheel/touchmove
listener whose `canMove` gate excludes only the EXACT clamp, never the band — so a park can be created
AFTER the gesture's last scroll event, and the listener that would have armed its settle has already run
for the final time. The frame-arrival settle could not rescue it either: `_settleBottomPark()` demands
`atBottom()` exactly, and a rest one row short of the clamp is inside the band but not on it.
`preservesForegroundReaderHold` then deliberately MUTES the foreground-stall watchdog for
`native_scroll`/`wheel`/`touch`, so nothing escalated and nothing repaired. Equally wrong, and forbidden by
the entries above: widening `atBottom()`, widening the rAF clamp settle to the band, or resuming
synchronously on frame arrival — a `scrollTop` write mid-gesture cancels the scroll animation Chromium is
still running for the reader.

**Right** — **the guaranteed event recruits the settle; the settle still waits for quiet.** A parked
position-only reader resting inside the band but off the clamp asks for the scroll-idle window on every
applied frame (`CellGridRenderer._settleBottomPark()` → the injected `requestFollowBandSettle`), which the
pane wires to `ensureFollowSettle()`. Frames are the one event a stalled pane always has, so liveness no
longer depends on a scroll event that may never come; and because the resume still runs only
`BOTTOM_FOLLOW_SETTLE_MS` after scrolling goes quiet, it cannot land mid-gesture. `ensureFollowSettle()`
opens a window only when none is pending and `restartFollowSettle()` (the scroll path) is the only caller
that re-arms — a busy PTY delivering a frame every few milliseconds would otherwise defer its own resume
for as long as output continued. The rAF clamp settle keeps demanding `atBottom()` exactly and owns the
clamped case, so an in-band frame never routes a clamped follower through the 180ms window.

**Guard** — `crates/roost-web-terminal/tests/render_scroll_settle.rs` —
`a_band_rest_parked_with_no_scroll_event_recruits_the_settle_on_a_frame` (and asserts zero `scrollTop`
writes at frame arrival),
`a_stream_of_frames_over_a_band_rest_keeps_exactly_one_settle_window`, with
`a_park_beyond_the_follow_band_recruits_no_settle_window` and
`a_park_on_the_exact_clamp_resumes_on_the_frame_with_no_settle_window` as the boundary controls.

### A find park swallows the scroll that returns the pane to the bottom

**Symptom** — "used find, closed the find bar, scrolled back to the bottom, and the terminal is frozen until I
type"

**Wrong** — treating EVERY scroll event as sacred to the find anchor: `handleScroll()` capturing the anchor and
returning before the at-bottom resume whenever the reason is `find`. `closeFind()`
(`crates/roost-web-terminal/src/find.rs`) only clears highlights and query state — it never resumes the
reader — so before this change no gesture at any position could un-park the pane after a dismissal, and a box
change with scroll range remaining refused it too. Equally wrong: resuming on any at-bottom event regardless
of origin, which lets `scrollToScrollbackRow()`'s own write to a TAIL hit clamp onto the bottom and instantly
un-park the navigation the user just asked for — and, the same mistake one layer out, treating every NON-owned
at-bottom event as a gesture: a box grow that drops the scroll maximum below a near-bottom find reader makes
the browser clamp `scrollTop` and dispatch a scroll the user never performed, releasing the anchor park
`noteBoxResize` had just deliberately refused to touch.

**Right** — a user scroll onto the exact bottom is the universal return to live and means the same thing for
every reason, `find` included: resume explicitly, so the find bail is bypassed and the pin lands. Distinguish
origin from the facts the event itself carries: the renderer-owned epoch already computed at the top of
`handleScroll()`, plus the last observed scroll MAXIMUM (`scrollHeight - clientHeight`, recorded on every
observed event and on every pin, never in `noteBoxResize` — recording the shrunken maximum there would make the
clamp that follows look like a gesture, whatever the dispatch order). An `owned` event keeps the anchor it just
aimed at, and so does an event whose maximum SHRANK since the last observation; only a non-owned event on an
unchanged maximum is a return to live. A maximum of zero is never a clamp: with no range nothing can be aimed
at, so every park yields. Never classify this with a timer, a task-ordering flag or a ResizeObserver-to-scroll
handshake — that dispatch order is not guaranteed, so a mark set in the resize path can arrive after the event
it was meant to classify. Geometry events the user did not aim at the bottom
(`noteBoxResize`) still preserve a find park unless the box has no scroll range left. Dismissing the find bar
must NOT resume: it would yank a reader off the match they are still looking at.
The suppression is one-shot per SHRINK, not per gesture — the maximum is recorded before every classification,
so no event can leave a larger value behind and the next event on settled geometry resumes. An animation that
shrinks the maximum over consecutive frames (divider drag, mobile keyboard) therefore refuses an anchor release
once per frame; the position still moves, zero range removes the suppression outright, and the cost is bounded
to one event of latency for an anchor park whose last gesture event coincided with the final shrink frame.

**Guard** — `crates/roost-web-terminal/tests/render_find_park.rs` —
`a_user_scroll_to_the_exact_bottom_resumes_a_find_park`,
`a_renderer_owned_write_that_lands_at_the_bottom_keeps_the_find_park`,
`a_find_park_survives_a_scroll_that_does_not_reach_the_bottom`,
`a_box_grow_clamp_onto_the_bottom_keeps_a_find_park`,
`a_clamp_that_leaves_no_scroll_range_resumes_a_find_park`, and
`a_gesture_after_a_clamp_still_resumes_a_find_park`, which pins the one-shot property — making the record
conditional on `clamped` turns it red;
`crates/roost-web-terminal/tests/render_scroll_settle.rs` —
`a_wheel_park_clamped_onto_the_bottom_by_a_box_grow_resumes` for the position-only side.

### A dismissed find bar leaves the pane parked on a dead find anchor

**Symptom** — "closed the find bar and the terminal never came back to life"

**Wrong** — ending the find SESSION without ending the find reading INTERVAL: `closeFind()` clearing
highlights, query and matches while the renderer stays `readerIntent=reading readerReason=find`. The
anchor-owning park then outlives the feature that created it, and every generic recovery refuses it — a box
change with range remaining, and a hold release. Equally wrong: resuming from `closeFind()`, which pins and
yanks a user who dismissed the bar while reading a mid-history match.

**Right** — the renderer exposes `endFindReading()`, which downgrades the reason `find` → `native_scroll` and
touches nothing else: no scroll write, no pin, no frame applied, position preserved. `closeFind()` calls it
last. After dismissal the park is an ordinary scroll park, so an exact-bottom scroll, a zero-range grow, a hold
release or the bottom-clamp settle all resume it, while an OPEN bar keeps anchor semantics.

**Guard** — `crates/roost-web-terminal/tests/render_find_park.rs` —
`closing_the_find_bar_ends_the_park_without_moving_or_painting` and
`a_dismissed_find_park_follows_a_box_grow_its_anchor_would_have_refused`.

### A paint hold armed on an edge outlives the listener that would clear it

**Symptom** — "terminal never paints again after a UI layout change / typing reaches the PTY but the grid is
frozen", with `hold_mask {selection: true}` while nothing is selected anywhere on the page, or
`hold_mask {link: true}` after a modifier keyup that was delivered somewhere else, and a scroll to the exact
bottom returning `{reconciled:false, anchorChanged:false}`.

**Wrong** — arm `RENDERER_HOLD_SELECTION` edge-only from the document `selectionchange` listener and re-attach
that listener without re-deriving the hold. The pane detaches its global listeners for the whole of a withdraw
(`crates/roost-web/src/components/terminal/cell_terminal.rs`), and a transient layout gap routes through
`parkViewAfterLayoutGap()`, which deliberately does NOT `releasePaintHolds()` — so a selection dropped inside
that window pins a hold no selection justifies. Equally wrong: dropping holds in the layout-gap park (it exists
so jitter does not re-mint every other viewer's geometry, and it would discard a real reader's selection), or
adding a renderer watchdog to guess the hold away, or deriving a modifier level UP on a pointer event while
only ever lowering it on the keyup edge.

**Right** — **holds are LEVEL-derived from the live document, not latched on an edge.** The attach transition
re-runs `syncNativeSelectionHold()` (untracked, so the gate does not subscribe to the presentation refresh it
performs), which is the single evaluator of the hold; a still-live selection therefore keeps holding and a
vanished one stops. `_resumeLive` refuses a held pane BEFORE mutating reader state, so the pane keeps reporting
its real intent/reason and `reconcile_block_reason="selection_hold"` instead of a lying `live`/`null` — which
also keeps the foreground-stall watchdog muted instead of redialing a view the mask refreezes, and makes the
eventual hold release pin the bottom (`pinOnResume`) as its `selection` reason intends. Any non-zero hold mask
is a total paint kill: frames are accepted and swallowed, so no scroll can heal it.
The link hold is level-derived the same way: every container pointer event carries the LIVE modifier state, so
`mouseover`, `mouseenter`, `mousemove` and `mousedown` each re-derive `armed` in BOTH directions
(`crates/roost-web-terminal/src/links.rs`), and that predicate must be TOTAL — an event with no modifier
fields must read as "not held", never `undefined`, or the hold ends up neither armed nor disarmed. Re-entering
a pane with nothing held can no longer revive a hold from a dead edge.

**Guard** — `crates/roost-web-terminal/tests/selection_guard.rs` —
`a_selection_dropped_while_the_panes_listeners_are_detached_stops_holding_paint`,
`a_selection_still_live_when_the_listeners_re_attach_keeps_paint_held`;
`crates/roost-web-terminal/tests/terminal_links_gestures.rs` —
`the_modifier_level_is_total_and_an_event_without_modifier_fields_is_not_held`;
`crates/roost-web-terminal/tests/terminal_links_armed_hold.rs` —
`re_entering_the_pane_without_the_modifier_cannot_revive_the_hold`.

### A reader hold declining the DOM deadline retires the pane's only repair

**Symptom** — "the dot is amber and the terminal paints nothing until I type" / "only a reload fixes it",
with `catching_up` held indefinitely, `onCatchUpStalled` observed over and over with no effect, and
`cell.foreground_stall` never firing again for that session.

**Wrong** — `recoverUnreconciledDom` returning on a reader hold — or a hidden page, an inactive view, an
already-reconciled watermark — while `domReconciliationWatermark` stays SET. The 3s timer nulls
`domEscalationTimer` before invoking the callback, so nothing is armed behind that target, yet
`handleCatchUpStalled` early-returns for as long as the target is non-null. The pane-local repair net
latches OFF for the rest of the foreground episode. The trap is reading a hold as a reason to "keep the
target for later": nothing ever revisits it.

**Right** — **a declined recovery leaves no armed target behind**, because the stall gate reads
target-presence as "recovery already owns this repair". Only the stale-callback check
(`domReconciliationWatermark !== watermark`) is a bare return — a newer target owns that state and must not
be cleared. Every other decline calls `clearDomReconciliationTarget()` first, so the next
`onCatchUpStalled` arms a fresh target once the hold lifts. The decline releases ONLY the target: it never
escalates, redials, pins, resumes, or otherwise touches the held reader's paint park. Both gates read one
predicate (`domRepairStillWarranted`) so the admitting facts cannot drift apart.

**Guard** — none — the v2 guard `apps/web/tests/cellTerminalPresentation.test.ts` left with the TypeScript tree.

### The painted scroll space describes only the shipped tail

**Symptom** — "scrollbar thumb size/position jumps with no user action / reader lands on a different row after a tab switch or re-attach / scroll bar 'all over the place'"

**Wrong** — anything that writes `scrollTop` to compensate — intent state, reveal correction, restoring a
remembered row — all still forbidden by the single-writer entry; equally wrong: shipping the whole ring in every
full frame (forbidden by the pull-backfill entry) or just making the shipped tail bigger, which only moves the
lie.

**Right** — **the painted scroll space must represent the WHOLE session history, not just the painted rows.**
`CellGridRenderer` reserves the unpainted `[0, sbBase)` history as a `.cell-sb-spacer` SIBLING of
`.cell-scrollback` (`_syncSpacer`, called from append, prepend and the `fonts.ready` hook), so an absolute row
index has a FIXED pixel offset for the epoch: prepends shrink it by exactly what they paint, evictions grow it
by exactly what they drop, and a reframe repaints the same rows at the same offsets — native `scrollTop`
therefore preserves the reader's row across all three with ZERO application scroll writes, and the thumb
reflects the real total. Sibling placement is load-bearing: eviction takes `scrollbackEl.firstElementChild` as a
block, and `nearHistoryTop()` reads `scrollbackEl.offsetTop` — which now includes the spacer, so a reader who
drags into reserved-but-unpainted space keeps the backfill drain pulling toward them.

**Guard** — `crates/roost-web-terminal/tests/render_history.rs` —
`the_spacer_reserves_the_unpainted_history`,
`a_head_page_shrinks_the_spacer_by_exactly_the_rows_it_adds`,
`an_eviction_grows_the_spacer_by_exactly_the_rows_it_drops`;
`crates/roost-web-terminal/tests/render_geometry.rs` —
`render_full_reserves_the_incoming_spacer_before_wiping_painted_history`.

### Reveal after dormancy loses unchanged cells

**Symptom** — "tab switch or browser reconnect shows only cells that changed
while hidden; static TUI chrome stays blank until a full repaint"

**Wrong** — make `CellTerminal` own the baseline, deliberately discard hidden
frames, or use a renderer watermark or zero-byte reveal witness to guess that
its old DOM is still authoritative. Component, socket, coordinator membership
and worker stream lifecycles do not expire together.

**Right** — `terminal-stream.ts` owns one canonical replica per session while
any view handle exists. Renderer detach does not delete it. Explicitly inactive
views stop constraining SCD and receiving cells; reactivation starts an
independent per-socket snapshot cursor from the coordinator cache, or waits for
the worker's full when the stream/geometry changed. New-stream cells share the
same scheduler lane as their state predecessor and cannot overtake it. A
same-epoch/same-width repair updates the live tail without deleting already
painted immutable history or the reader's global anchor.

**Guard** — `crates/roost-client-core/tests/terminal_full_before_delta.rs`;
`crates/roost-web-terminal/tests/render_history_repair.rs` —
`a_live_viewport_only_full_preserves_painted_history_and_inserts_only_the_missing_tail_gap`.

### Reveal lands in history instead of the present

**Symptom** — "tab switch lands in scrollback / watches history paint top-down / a stale pane reveals mid-history or in blank space and crawls to the bottom 250 rows per round trip"

**Wrong** — zero the claim's held boundary for bottom-followers (worker returns a plain tail → the tail merge
yields null → a full repaint wipes painted rows, the reader clamps into the stale spacer, `nearHistoryTop()`
starts a top-down drain); let geometry changes silently unlatch bottom-follow (box shrink/grow while parked, the
800×600 park fallback, spacer synced AFTER the content wipe); mount the pane under per-screen `<Route>` entries
so a `/file` or `/search` visit remounts the whole deck cold.

**Right** — **a reveal lands on the present, always at the literal bottom.** (a) the claim snapshot is
viewport-only and history is refilled behind the reader (see the epoch-addressed entry below); (b)
`noteBoxResize()` re-pins a reader who was at the OLD box's bottom (`max(prev,next)` covers shrink+grow;
ResizeObserver calls it BEFORE the drag gate); (c) a full repaint syncs the spacer BEFORE wiping painted content
so the scroll max never dips under `scrollTop`; (d) the slow path pins to bottom when the incoming base is past
the held window (no image in the new epoch — collapse allowed, bottom mandatory); (e) ONE route definition for
all MainPane screens plus an always-mounted deck host (visibility flip) so `/file` and `/search` never tear the
deck down. This was the 6th attempt at this class: the prior five "passed" because nothing asserted what the
reader SEES at first paint, so the smoke now samples the READER'S POSITION during reveal.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### Reveal waits on history before the live bottom is readable

**Symptom** — "tab switch / reveal waits on history before the live bottom is readable / deep sessions reveal slower than shallow ones"

**Wrong** — bridge a renewal to the viewer's entire held boundary, or
proactively refill retained history after reveal. Either form makes resume work
scale with session depth and mutates the painted grid without reader demand;
equally wrong: racing history away or reordering a mixed history+viewport
repaint (breaks the single scroll writer).

**Right** — every authoritative FULL is viewport-only and epoch-addressed: no
scrollback rows, a base equal to the total, and an opaque `gridEpoch`.
The renderer installs the current viewport and truthful spacer immediately.
Only explicit scroll/find demand fetches disjoint
`SessionsGetScrollbackCells` ranges carrying that epoch
(`crates/roost-coord/src/terminal_screen/scrollback_relay.rs` relays it); the worker
checks the epoch before and after each cooperative slice and returns an error
rather than splice re-numbered rows. While the reader is off-bottom, every FULL
frame — including an epoch change — is retained off-DOM as the latest pending
frame and deltas fold into it; the painted frame, spacer, `scrollTop` and
visible row stay immutable until an explicit return to bottom applies the
latest frame once. If the worker ring dropped the requested prefix, the shorter
response's start row is the retained floor: paint the surviving suffix and
park there rather than rejecting the page or re-requesting impossible rows.
Paused Sync recovery resumes the mounted loop in place (no reload), and durable
replay yields periodically so live cells preempt it.

**Guard** — `crates/roost-web-terminal/tests/render_append_frames.rs` —
`a_viewport_only_full_reserves_depth_and_explicit_pages_fill_the_seam`.

### The painted grid never converges until a reload

**Symptom** — "terminal keeps running but the painted grid never converges until a reload / typing reaches the PTY while the pane stays frozen / a returned-to pane paints an old frame forever / 'only a refresh fixes it'"

**Wrong** — patch whichever layer is in front of you: cancel the reader on passive output (or never end its
interval when the pane parks), re-derive the viewport claim from component-local liveness flags, park Sync
permanently after N failed dials and wait for the user to reload, treat an unproven worker result as a rejection
and roll the viewer's cell subscription back, let the announcement barrier drop cells out of order and hope a
later delta re-syncs, or keep inferring the core's scrollback eviction origin and emitting phantom continuation
cells — each one leaves the canonical model ahead of the DOM with nothing that MUST repair it.

**Right** — **six layered contracts, each with one owner and a typed outcome.**

- (a) Reader intent is explicit: `CellGridRenderer` holds `ReaderIntent` "live"/"reading" plus a composed
  selection+link hold mask (`crates/roost-web-terminal/src/cell_renderer.rs`); passive output and composer drafting never
  cancel a reader, one admitted local keystroke calls `prepareLiveInteraction()` (clear holds + adopt
  reader-pending frame + re-pin bottom as ONE transition), and park/`pagehide`/unmount ENDS the reading interval
  so a revealed pane presents the newest canonical frame.

- (b) `terminal-stream.ts` owns one per-session browser replica and stable view
  handles. `CellTerminal` only measures, publishes active/inactive geometry,
  forwards attributed input and attaches a renderer. Detach never destroys the
  baseline; a reconnect replays desired views and resumes from a full snapshot.

- (c) `TerminalViewHub` is the only membership/SCD owner, and membership is not
  geometry. It independently minimizes columns and rows across the views that
  are actually looking (`minimumTerminalGeometry`, `@roost/protocol/viewport`).
  Park retains MEMBERSHIP for reclaim until the lease expires, but a parked
  view stops constraining GEOMETRY once `TERMINAL_VIEW_PARK_GRACE_MS` lapses;
  with no live viewer left the last effective geometry is HELD rather than
  re-minted, so a solo viewer's blip never tears the stream down. It mints a
  UUID stream for every effective geometry or worker-generation transition.
  Invalid or fail-closed worker outcomes retain membership but publish
  unavailable until route reconciliation can issue a fresh stream.

- (d) The worker owns one generation-addressed stream state per session. The
  keeper's resize ACK is the ordered parse boundary; the existing wterm core is
  resized synchronously there, never rebuilt for an ordinary live resize. Input
  has its own lane and worker-owned keeper correlation keys, so browser-local
  sequence collisions cannot replace another device's pending result.

- (e) `TerminalScreenHub` validates and folds full/delta frames into one
  canonical coordinator replica, assembles bounded row chunks atomically, and
  latches one resync on any gap, invalid frame or ten-second chunk stall. Each
  socket owns an independent snapshot cursor and delta tail on the same
  per-session scheduler lane as view state.

- (f) Sync redial caps delay rather than attempts. A hidden document may sleep,
  but a lifecycle wake reconnects in place, replays active view intent and
  converges from a complete baseline without a reload. Durable session
  publication still commits before route installation and `sessionBus`
  publication.

Diagnose `wire_received` → browser `replica` → `handler_canonical` →
`dom_reconciled` plus `reconcile_block_reason`
(the smoke contract `smoke/contract/terminalDiagSnapshot.ts`), never a screenshot.

**Guard** — `crates/roost-protocol/tests/cell_grid_chunks.rs` —
`a_rejected_part_leaves_the_assembler_with_no_partial` and
`a_replacement_snapshot_must_start_at_zero_inside_the_same_stream`
(the protocol-side chunk assembler);
`crates/roost-worker/tests/terminal_stream_state.rs` —
`shrink_and_grow_resize_the_same_core_at_the_keeper_boundary`;
`crates/roost-coord/tests/terminal_view_geometry.rs`;
`crates/roost-coord/tests/terminal_screen_hub.rs` and `terminal_screen_hub_lifecycle.rs`;
`crates/roost-client-core/tests/terminal_full_before_delta.rs`.

### A busy session restarts its own baseline forever while chunks assemble

**Symptom** — "ordinary frame interrupted chunk assembly / attaching to a busy terminal never
finishes — the coordinator keeps requesting snapshots and `terminal.screen_resync` loops"

**Wrong** — aborting the in-flight chunked full and latching a resync because ANY ordinary frame
arrived mid-assembly. A session emitting deltas faster than its multi-megabyte baseline chunks land
restarts the transfer on every delta: the worker builds another full, the next delta interrupts it
again, and attach never completes.

**Right** — park ordinary deltas in a per-session bounded hold (`TerminalAssemblyHold` in
`crates/roost-coord/src/terminal_screen/hub_state.rs`: 512 frames / 4 MiB, mirroring the Sync v2
delta-tail caps) while chunks assemble. When the assembled full installs, replay only held deltas
whose base_seq extends the new baseline — earlier ones are already contained in that full — through
the ordinary delta fold. Overflow, any other interruption, invalidation, or a minted stream clears
the hold and falls back to the single-resync latch; an ordinary FULL still supersedes the partial
outright without a resync.

**Guard** — `crates/roost-coord/tests/terminal_screen_hub_hold.rs` —
`deltas_held_during_assembly_fold_as_if_the_run_was_never_interrupted`,
`a_hold_that_overflows_falls_back_to_the_single_resync_latch`.

### A newly minted terminal stream whose baseline never arrives hangs forever

**Symptom** — "the pane shows nothing and there is no indicator at all", typically right after another
viewer joined, left, parked or woke. The browser sits `accepted` with `baselineReady:false`, which
`deriveTerminalPresentationState` reported as `idle` — no dot — so nothing even looked wrong. Only a reload
recovered.

**Wrong** — `TerminalScreenHub.expectStream` calling `snapshots.reset(state, true)` (which cancels
`repair.requestTimer` and zeroes `requestAttempt`), installing `state.expected`, clearing `resyncLatched`,
and arming NOTHING. The repair ladder was only ever entered by an ARRIVING frame: the `acceptDelta` latch, a
chunk stall, or an invalid full. A worker transaction that commits `enabled` and installs no baseline sends
no frame at all, so nothing entered the ladder and the coordinator waited forever with `expected` set and no
cache. Stream re-mints are routine — `terminal-view-stream-controller.ts` mints a fresh `streamId` for every
desire, so any other viewer joining, leaving, parking or waking re-mints for everyone — which makes ONE
silent mint enough to freeze the session for the viewer that never moved. Equally wrong: papering over it in
the SPA with a browser-side snapshot timer. The coordinator is the only party that knows which stream it
expects.

**Right** — **the party that mints a stream owns proof that its baseline arrived.** `expectStream` arms a
first-byte deadline for the stream it just minted (`SnapshotRepairState.baselineTimer` +
`TerminalScreenSnapshotController.armBaselineTimer`, `TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS` through the
injected `setTimer`). On fire it re-validates exactly as `startSnapshotRequest`'s timer does — same `state`
object still in `sessions`, unchanged `repair.generation`, unchanged `expected.streamId` — and stands down
when the baseline already landed (`cache.valid`) or a chunked transfer is in flight, because the chunk stall
timer owns that case; otherwise it calls the existing `latch`, which runs the established snapshot-request →
two-attempt → `requestFreshStream` ladder. Exactly ONE first-byte deadline exists per session: `complete()`,
`reset()`, `armChunkTimer()` (a chunk IS the baseline arriving) and `startSnapshotRequest` (once the request
timer owns the deadline) all cancel it. The transition emits
`log.warn("terminal-screen", "baseline_timeout", …)`, so a silent worker is readable in `*.err.log` instead
of being inferred from a blank pane.

**Guard** — `crates/roost-coord/tests/terminal_screen_hub_snapshot.rs` — escalation to a fresh stream across
both ladder attempts, zero requests when the baseline lands in time, a re-mint replacing the superseded
deadline, a chunked transfer in flight at the deadline left to the chunk stall timer:
`a_stream_whose_baseline_never_arrives_climbs_the_repair_ladder`,
`a_baseline_that_lands_before_the_deadline_asks_for_nothing`,
`a_reminted_stream_deadline_replaces_the_superseded_one`,
`a_baseline_mid_transfer_is_left_to_the_chunk_stall_deadline`.

### A seeded attach sends its baseline before its view-state, so the second viewer paints nothing forever

**Symptom** — "a second browser page opens a session that is already streaming and the pane stays blank
forever". `__smoke.terminalBrowserSnapshot` reads `view.status: "accepted"` with a populated `stream_id` and
`replica.expected_stream_id` set, but `wire_received` all-null, `cellFrameCount: 0`, and the pane sitting on
"View accepted at WxH; waiting for its full baseline". No `screen_resync` is ever emitted, so nothing in the
logs says a repair was needed. The smoking gun is on the wire: a `cell_grid` at `delivery_seq=4` followed by
`terminal_view_state` at `delivery_seq=5` on the cold socket, while the warm socket emits them the other way
round (view_state 2, cell_grid 3).

**Wrong** — `TerminalViewHub::apply_owner_view_state` calling
`socket.sink.seed_socket(...)` BEFORE `socket.sink.enqueue_terminal_state(...)`. A browser folds a cell only
against the stream its LAST view-state named, so a baseline that overtakes its own state arrives at a replica
that has been told nothing and is refused by `admit_frame` as `stale(token)`
(`crates/roost-client-core/src/terminal/session.rs:223`) — and per that function's own comment a stale frame
deliberately does NOT latch a repair, because a stale delta is not evidence of loss. The socket is now stuck
forever: refused frames, no resync, no timeout. The first viewer escapes it by luck rather than by design — its
`seed_socket` finds no resident cache (`has_cache=false`, `seeded=false`), so nothing is seeded and its
baseline arrives later with the worker's own full, which happens to restore the order. Only an attach that
actually seeds — the second viewer joining a live stream — is affected, which is why it reads as "the other
page is broken" rather than "terminals are broken". The lane already enforces this order for its own frames
(`ready_ring.rs:145-150` sends `pending_states` ahead of any cursor part; `send_queue.rs:281-296`
`terminal_priority_insert_index` inserts a state ahead of cells), so the violation was at the caller: it seeded
before the state existed, and nothing downstream could reorder them.

**Right** — put the view-state on the wire before the seed, so the stream the baseline belongs to is the one
the replica has already been told to expect. This is NOT the entry above, "a newly minted terminal stream
whose baseline never arrives": that one is a worker that installs NO baseline and sends no frame, which the
first-byte deadline and repair ladder exist to catch. This one SENT the baseline, correctly, and the client
refused it — so the ladder is not merely slow, it is never entered, and a watchdog that only watched for
silence would have passed this bug. Different cause, different fix, different guard; do not merge them.

**Guard** — `crates/roost-coord/tests/terminal_view_owner_screen.rs` —
`an_attach_that_seeds_sends_the_view_state_before_the_baseline` records the sink's calls in order and asserts
`["state", "seed"]` for a socket seeded from a resident replica.

### A refused view-command write leaves an actively-viewed pane unscheduled

**Symptom** — "the terminal stopped about fifteen seconds after a network blip and never came back on its
own", with the pane still visible and focused and no error anywhere.

**Wrong** — `publishIntent` calling `cancelTerminalViewRenewal(view)` when `sendSyncV2Command` returns
false. That write is refused for a non-OPEN readyState, a non-accepting link, or a throwing `ws.send` — all
transient — and `armTerminalViewRenewal` is reached ONLY from a successful publish, so the view left the
renewal set with nothing scheduled. The coordinator lease then expired at `TERMINAL_VIEW_LEASE_MS` and
frames stopped. No event follows a refused write on a still-ready link, so nothing republished.

**Right** — a view that still wants frames is never left with no scheduled attempt. The refused-write
branch re-arms through the EXISTING renewal scheduler (`TERMINAL_VIEW_HEARTBEAT_MS`, one third of the
lease, so the lease is renewed before it can expire), and still cancels for a view that wants no frames —
disposed, inactive intent, or a hidden page, where stopping the heartbeat is the correct behaviour. The
no-publication-target branch deliberately still cancels: `retargetSession` republishes on the
generation/ready flip and `cell-terminal-lifecycle` republishes on the visibility transition, so that class
already has a guaranteed exit and a re-arm there would be a redundant poll.

**Guard** — none — the v2 guard `apps/web/tests/terminalStreamLifecycle.test.ts` left with the TypeScript tree.

### The liveness watchdog deletes itself on the failure it exists to notice

**Symptom** — "the pane just stops and nothing ever retries", with no `cell.foreground_stall` line for that
session after the first one.

**Wrong** — `armTerminalForegroundIdleProbe`'s callback discarding the boolean from
`requestTerminalLivenessChallenge` after having already nulled `session.idleProbeTimer`. That call returns
false when there is no publication target, when `expectedStreamId` is unset, or when the resync command
cannot be written — so on exactly the failure that means something is wrong, the session was left with NO
probe and NO proof deadline until a frame happened to arrive. Same shape one level down:
`sendLatchedTerminalResync` armed a repair only while `resyncLatchedAtMs !== null`, and an accepted delta
nulls that field, so a latched replica could end up with no proof deadline either.

**Right** — every exit from the probe callback arms a proof deadline, re-arms the probe, or retires
liveness because nothing is viewed. A challenge that cannot be published re-arms at the same interval,
re-anchored at `now` so a stale frame timestamp cannot produce a 0ms spin. A latch with no pending
challenge for its owner arms a repair regardless of the latch timestamp; the pending-challenge gate remains
the sole coalescer, so concurrent view renewals still cannot multiply repairs.
The retry is reported as an EPISODE, never per retry: `signal()` is Tier-1 and a line every other probe
(~360/hour per stuck session) would leave `roost doctor` permanently red and drown the channel it exists to
serve. `probeRearmReported` carries that edge, and EVERYTHING that ends an episode must clear it — a
published challenge AND `clearTerminalSessionLiveness`, because a flag surviving retirement silences the
first rearm of the next episode, which is the only one that reports. The rearm keeps its own cooldown scope
so it cannot coalesce away the resync or redial that follows it.

**Guard** — `crates/roost-client-core/tests/terminal_liveness_retirement.rs` —
`an_unpublishable_challenge_re_arms_from_the_current_instant_and_never_spins`
(two further sweeps at the SAME `now_ms` must leave the re-armed deadline
identical, which is what a stale-anchor spin fails),
`a_second_request_for_one_gap_re_anchors_its_proof_and_publishes_no_second_challenge`
for the coalescer, `park_rotation_stream_generation_and_page_hide_each_retire_both_deadlines`
for the five transitions that must retire both deadlines, and
`the_rearm_episode_reports_once_and_reopens_after_both_of_its_ends` for the two
ends that must clear the episode edge — a published challenge AND a hidden-page
retirement — so a report suppressed by a surviving flag fails on the second
episode rather than on the first;
`crates/roost-client-core/tests/terminal_foreground_liveness.rs` —
`a_quiet_foreground_pane_challenges_and_the_painted_full_proves_it` and
`an_unanswered_proof_invokes_the_existing_sync_generation_recovery` for the
challenge → proof → escalation ladder itself. The state machine is
`crates/roost-client-core/src/terminal/liveness.rs` and the sweep that fires it is
`crates/roost-client-core/src/handle_sweep/liveness.rs`.

### A baseline that never arrives leaves the pane expecting a stream with nothing behind it

**Symptom** — "the second viewer joined and my pane went blank", or it kept the
old grid while the pane beside it re-based. The console shows one line,
`expecting a fresh baseline`, and then nothing for as long as the operator
watches: no `replica gap latched`, no refused frame, no `cell.foreground_stall`,
and `repair_attempts: 0` with `repair_outcome: "none"`. Measured at a two-viewer
transition — the coordinator seeded the incumbent's socket twice on the new
stream and `replace_terminal_snapshot` returned `true` both times, so the baseline
part was queued and then dropped on the browser side.

**Wrong** — arming the foreground idle probe only when a baseline is already
installed (`if (session.baselineReady) armTerminalForegroundIdleProbe(session)`).
The pane that needs the probe is precisely the one whose baseline never arrived:
nothing was refused, so `requestTerminalResync` never fired, the repair latch
never latched, and a second viewer narrowing the session — which mints a new
stream for everyone — leaves the incumbent replica expecting the new stream while
it still paints the old one. v2 does not read as missing this rule because a
SECOND timer covers the shape: the view lease's `requestTerminalLivenessChallenge`
at `terminal-stream-view-commands.ts:383`. Moving the watchdog to a sweep, where
one owner runs every deadline, loses that timer unless the arm condition moves
with it.

**Right** — arm the probe on the EXPECTATION and the foreground view, never on a
baseline: an expected stream and an active view is the whole condition, and the
anchor is the sweep that first noticed rather than a frame that never came. The
challenge is the same scoped resync every other repair uses, and it carries an
empty `grid_epoch` with `seq: 0` — a pane with nothing painted has no checkpoint
to name, which the authority already reads as "send me anything" and answers from
its resident cache.

**Guard** — `crates/roost-client-core/tests/terminal_foreground_liveness.rs` —
`a_pane_whose_baseline_never_arrived_is_challenged_without_a_frame_ever_landing`,
which never admits a frame at all and asserts the challenge names
`(stream, "", 0)`. The control that the widened arm does not challenge a pane
that IS painting is
`a_foreground_view_that_keeps_painting_is_never_challenged`.

### The pane's re-claim net only protects a pane that never painted

**Symptom** — "it was streaming, then it stopped, and nothing tried to reconnect it" — no re-claim, no
notice, no diagnostic.

**Wrong** — feeding `offlineWatch.update(viewed, hasReconciledFrame())` where `hasReconciledFrame` is set
true once on the first reconcile and never reset. `createOfflineWatch` disarms whenever `hasFrame` is true,
so the 3s grace and its two silent `view.refresh()` re-claims covered ONLY a pane that had never painted —
the precise inverse of the reported failure.

**Right** — the net takes two liveness FACTS instead of a one-way latch: the view is undeliverable while
the operator is looking at it, and no frame has painted recently. Accusation requires both, sustained past
the grace. Output silence alone may NEVER accuse: a shell with nothing to print is silent indefinitely, so
the undeliverable-view fact is the only admissible evidence, and it reuses the presentation layer's
existing `detached` determination rather than inventing a second notion of "broken".
`FRAME_ACTIVITY_WINDOW_MS` (500ms) is deliberately shorter than `DETACHED_GRACE_MS` (1000ms) so freshness
cannot still be true at the edge that arms the re-claim and mask it.

**Guard** — `crates/roost-web/tests/terminal_pane_presentation.rs` —
`a_detached_viewed_pane_re_claims_twice_before_it_is_offline`, with
`a_quiet_but_deliverable_or_unviewed_pane_is_never_accused` as the false-positive control.

### A composer suspension holds paint forever when its restore never runs

**Symptom** — "typing reaches the PTY but the grid is frozen" with `hold_mask {selection: true}` while
nothing is selected anywhere on the page — the same symptom as the level-derived-holds entry above, from
the one hold that was still latched.

**Wrong** — `selectionGuardSuspended` as an unconditional boolean forced into `held`, set by
`guard.suspend()` and cleared only by `restore()` / `release()` /
`discardActiveSelectionGuardForTransition`. If the suspending caller is torn down, its keyup or focus event
is delivered elsewhere, or the pane's listeners are detached across the window that would have restored it,
the pane holds paint for good. A timer was rejected as the fix: a held key legitimately keeps one
suspension open for as long as the user holds it, so any bound short enough to cap the wedge cancels a live
composer edit.

**Right** — the suspension is LEVEL-derived like every sibling hold. It keeps holding only while every fact
it was suspended FOR is still true: the captured range still validates by row identity, no other owner has
established a non-collapsed selection, this guard is still the active guard, and the element that owned
focus at suspend time is still `document.activeElement` and still connected. When any conjunct fails,
`syncNativeSelectionHold()` simply stops reporting held — no explicit clear required — and the lapse emits
`cell.selection_yield_lapsed` with its reason. A suspension taken while nothing was focused has no owner to
wait for and never holds paint at all, while `suspend()` still performs its range yield so no keystroke is
swallowed.

**Guard** — `crates/roost-web-terminal/tests/selection_guard.rs` —
`a_suspension_whose_restore_never_runs_stops_holding_paint_once_its_range_is_gone`, with
`a_suspension_whose_range_is_still_live_and_restorable_keeps_paint_held` as the refusal control that keeps
the composer contract.

### A selection reveal re-enters wasm and the panel paints the error instead of the frame

**Symptom** — "Error: closure invoked recursively or after being dropped" in the console, **exactly once per
selection reveal, and never otherwise**. It appears only while a selection is being HELD — dragged out, or
captured and parked across a patch — and disappears the moment the hold lapses. Nothing about the terminal's
content, size or liveness changes; the grid simply stops painting on that surface and the error is the only
evidence. The distinguishing read is the multiplicity: a re-entrancy fires per reveal, a freed registration
fires per delivery of whatever event it registered for.

**Wrong** — holding a `RefCell<CellGridRenderer>` borrow across `Selection.removeAllRanges()`. Chromium is
free to turn that call into a scroll, and the scroll dispatches into wasm while the renderer's `RefCell` is
still mutably borrowed; wasm-bindgen reports the re-entrant call with the same string it uses for a dropped
closure, which sends you looking for a lifetime bug that is not there. Equally wrong, and the tempting fix:
clear the reading hold on a timer, or release the selection earlier. **A held selection is SUPPOSED to withhold
a repaint** — that is what the hold is for, and clearing it to quiet a re-entrancy error trades a deliberate
no-paint for a visible wrong-paint. Do not "fix" this by suppressing the hold.

**Right** — **ownership and borrow scope, not the hold.** The renderer is not something a caller should hold a
mutable borrow of across a DOM call that can dispatch; give the reveal the work it needs without keeping the
`RefCell` borrowed over `removeAllRanges()`, and let the hold stand for exactly as long as its own rule says.
This is the other half of the wasm-bindgen error string documented under "Browser platform reality" in "a
`Closure` handed to the DOM as a raw function reference is freed while it can still be called", which is the
DROPPED half and a different defect with a different fix; both surface as the same console line, and the stack
and the multiplicity are what tell them apart. It is also not the "a composer suspension holds paint forever
when its restore never runs" entry: that is a hold that outlives its reason and must lapse, while this is a
hold doing its job and a borrow scoped too wide around it.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed. `PaneSelection` needs a real
`web_sys::Document` to hold a real `Selection`, so nothing about this re-entrancy can be constructed off a
browser target, and any test claiming otherwise would be testing a mock.

### A trapped resize capture suppresses a channel's emission for good

**Symptom** — one terminal stops producing frames permanently while its siblings on the same worker are
fine; a fresh stream for that session changes nothing and only a respawn recovers it.

**Wrong** — `failCore` leaving the per-channel `cellEmissionGates` entry SET and the live resize capture
ATTACHED. The gate exists to suppress emission for the duration of a capture and `finishCapture` was its
only non-teardown clear, so a capture that can never finish held the gate for the life of the channel.
`applyTerminalStreamState` then copied that dead capture into every later generation, so minting a new
stream inherited the suppression instead of escaping it.

**Right** — one owner for the rule that a capture which stops owning the channel hands back BOTH its
stream slot and the per-channel emission gate (`releaseResizeCapture`, called with a reason from the
boundary-applied paths, from `failCore`, and when a new generation drops a dead capture — dropping it
without releasing the gate would re-create the leak, because the gate is per-channel and outlives the
discarded generation). The release is refused only while the stream has already replaced this capture with
another LIVE one, whose boundary is still unproven. Fail-closed is unchanged: `coreValid` still gates
emission, so a trapped core refuses to build frames for the generation it trapped, and the release and the
invalid-core mint both log instead of passing silently.

**Guard** — `crates/roost-worker/tests/terminal_stream_core_trap.rs` —
`a_trapped_resize_reports_a_reprovable_core_and_releases_its_gate`, with
`a_generation_minted_after_a_trap_that_cannot_be_reproved_stays_closed` as the fail-closed control.

### A fail-closed stream verdict no new worker can clear

**Symptom** — every viewer of one session sees an unavailable pane for the rest of the coordinator's life;
restarting the worker does not help and nothing in the logs changes.

**Wrong** — `unavailablePolicy === "never"` with no door at all: `classify` assigns it for genuine
invariant failures (mismatched stream result, committed geometry mismatch, invalid request) and
`reconcileRoutes` then skipped such a session unconditionally while `redrive` and `redriveFreshStream`
refused too. Equally wrong, and the reason the blanket skip was written: retrying a protocol violation on a
timer, which hammers a broken worker and hides the violation.

**Right** — fail-closed stays; it becomes ATTRIBUTABLE. The verdict is stamped with the worker generation
that was current when it formed, and a reconcile clears it only when the reconciling fingerprint's observed
generation is strictly newer — a genuinely different participant, not a reconnect. Generation identity is
the routable `WorkerHandle` the dispatcher already fences on, counted only when the handle DIFFERS (a
connection re-announcing its own fleet snapshot repeats `workerReplacement` without being new), and a seam
that exposes no handle never earns a generation, so an unobservable participant leaves the pane down. No
timer and no retry budget: `onWorkerConnected` → `workerReplacement` is the guaranteed delivery. The
clearing logs the old and new generation, and every `terminal.stream_invariant_failure` /
`terminal.stream_result_mismatch` diagnostic is retained.

**Guard** — none — the v2 guard `apps/coord/tests/terminal/view/terminal-view-hub-worker.test.ts` left with the
TypeScript tree.

### A terminal never repaints again after the device that opened it went away

**Symptom** — a session opened on a phone is reopened on a desktop a day later and the grid is "frozen in
place", still sized to the phone; the live screen never repaints, and nothing short of a worker restart or a
reload that happens to hydrate recovers it.

**Wrong** — two independent unbounded latches, either of which alone produces exactly that pane.
(a) `stream.coreValid = false` is set by `failCore` (an unprovable resize boundary) AND by
`installStreamBaseline` (a canonical full that cannot be encoded, `terminal.invalid_frame`), and every later
generation inherits it (`coreValid: current?.coreValid ?? true`), so `applyTerminalStreamNow` short-circuits
before any resize, both emission gates refuse, and `classify` files the verdict as
`unavailablePolicy: "never"`. The only clears were session close and worker restart — a new device, a reload
and a resize all re-hit the latch.
(b) `registerDomainHydrator.run()` awaited its hydrator with no deadline while the Connect transports carry
no `defaultTimeoutMs` and no signal, so a request that never settles (a pooled half-open connection after a
sleeping laptop wakes) reached neither `.then` nor `.catch`, scheduled no retry, and left `domain.ready`
false for the session's life — no publication target, no view command, no ACCEPTED stream, and every frame
of any newer stream silently dropped against a stale `expectedStreamId`.

**Right** — (a) an unprovable core is RE-PROVED in place from the keeper's ordered history whenever a
stream desire reaches it (`crates/roost-worker/src/session/core_reprove.rs`, spliced into the invalid-core
rung of `applyTerminalStreamNow`), so the resize lands and a fresh full baseline paints. The rebuilt window
mints a NEW `gridEpochBase`, because re-derived history must make browsers renumber instead of merging into
retained rows. A fresh trap spends exactly ONE re-proof attempt on itself — the trap is the attributable
event with guaranteed delivery, `retry === 1` bounds it, and a second `core_failed` falls through to the
unchanged `"never"` verdict; no timer and no admission-time door, per the entry above. A capture that
already recorded a trap reports `core_failed` directly instead of re-running the lost-ACK recovery, which
would re-fail the same capture behind another keeper history read and hide that repairable verdict.
The repair is keyed on the LATCH, not on the producer, so an unencodable-baseline latch also spends one
attempt: the rebuild resets `sentFull` and the emitter state, and a second failure lands on the unchanged
`"never"` verdict exactly as before.
(b) the hydration deadline lives with the retry ladder it feeds (`SYNC_HYDRATION_DEADLINE_MS`), aborts the
request, and converts the silence into the ordinary rejected-snapshot retry; every hydrator threads the
`AbortSignal` into its RPC so the abandoned call is actually cancelled.

**Guard** — `crates/roost-worker/tests/terminal_stream_core_trap.rs` —
`a_fail_closed_core_is_reproved_from_keeper_history_on_the_next_desire`;
`crates/roost-worker/tests/terminal_view_owner.rs` —
`a_trapped_core_re_proves_itself_on_the_desire_the_trap_triggers` (the desire COUNT is what proves it is
not a loop) with `a_trap_the_keeper_cannot_re_prove_stays_fail_closed_and_desires_nothing_more`;
`crates/roost-client-core/src/sync/hydration.rs` — `a_call_past_its_deadline_expires_once`.

### A pane keeps a fallback font's cell advance and clips its own right edge

**Symptom** — "the terminal is wider than the pane on mobile / right-hand output is cut off and
unreachable / the live screen extends past what I can see"

**Wrong** — invalidating the cached cell box on `document.fonts.ready` only while the pane is
publishable (`!viewport.shouldPublishActive()` checked BEFORE zeroing `runtime.cellWidth` /
`runtime.cellHeight`), and observing only the `ready` promise captured at mount. Terminal webfonts
are `font-display: swap`, so a hidden, inactive, or pending pane measures the FALLBACK face, keeps
that advance forever — `measureViewport` re-measures only when a cached dimension is zero, and
reveal, box-resize and admission all republish without invalidating — and a narrower fallback
advance overclaims columns. `.cell-grid .cell-viewport` paints exactly `cols × 1ch` with horizontal
overflow hidden, so the surplus columns are painted outside the clip and cannot be scrolled to.
Also wrong: reaching for horizontal scrolling, automatic font shrinking, a second geometry
calculator, or a snapshot retry — those hide an overclaim the pane never had the right to make.

**Right** — `mountCellTerminalLifecycle`'s `onTerminalFontsSettled`
(`crates/roost-web/src/components/terminal/pane_mount/browser.rs`) returns only for `lifecycleDisposed ||
runtime.unmounted`; it zeroes the cached cell box and calls `renderer.invalidateRowHeight()`
unconditionally, and gates ONLY `publishViewportNow()` on `shouldPublishActive()`. A background pane
therefore claims nothing while its font settles and measures the loaded face on its next claim. The
same callback is registered on the FontFaceSet's `loadingdone` and `loadingerror` (removed in
`dispose()`), because `ready` answers one loading epoch: a face that starts loading later settles
through those events alone, and a failed download still means re-measuring whatever face paints.
The renderer's own `fonts.ready` hook repairs history placeholders and bottom placement — it is a
different responsibility, not a substitute for invalidating the lifecycle's cell cache. A text-size
change is the same invalidation with no font event at all: `term_font_size::apply_term_font_size`
raises `TERM_FONT_SIZE_EVENT` on `window`, and the pane routes it to the same callback — without
it a larger size keeps the old column count and paints past the clip until the next reload.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

---

## Terminal input, focus and keys

### Typing goes nowhere on a fresh mount

**Symptom** — "can't input anything in terminal on fresh mount / cursor blinks but typing goes nowhere / focusedClass=false even though textarea looks focused"

**Wrong** — rely on `.focus()` alone to fire focus events (it does not if the textarea was already
`activeElement` from a prior mount) / skip the mousedown click-recapture handler.

**Right** — the input textarea is off-screen, so clicks land on row spans, not the textarea; without an explicit
dance the focus listener never sees the event → the pane never reports focused → keystrokes go nowhere. The fix
lives in the renderer's input controller (`crates/roost-web-terminal/src/input.rs`, `forceFocus()`), and three pieces are load-bearing: (1)
`if (activeElement === textarea) textarea.blur()` BEFORE focusing — guarantees a fresh native focus event even
when the textarea was pre-focused; (2) an explicit `dispatchEvent(new FocusEvent("focus", { bubbles: true }))`
so pane styling is deterministic; (3) the container `mousedown` listener that calls `forceFocus` on every click.
Never leave the dance's re-focus guard latched on the error path or focus reporting dies for that pane's
lifetime.

**Guard** — `crates/roost-web-terminal/tests/input_controller.rs` —
`preserves_the_blur_focus_dance_and_removes_pane_local_listeners`,
`a_dance_whose_blur_fails_never_latches_the_focus_report_guard`.

### Bun.spawn does not inject TERM into the child env

**Symptom** — "backspace echoes wrong / Cmd-Backspace nukes prompt row / htop or vim crash with `ncurses: cannot initialize terminal type ($TERM=unknown)` — but ONLY on deployed workers, never on the local-bootstrapped one"

**Wrong** — assuming the PTY spawn sets the child's `TERM`. Bun's `Bun.spawn({terminal: {...}})` set the PTY's
internal `name` but did NOT inject `TERM` into the spawned child's env. The local worker inherited `TERM` from the
terminal that ran its original bootstrap; remote workers bootstrapped over non-TTY SSH inherited nothing → the
child shell sees `TERM=""`/`unknown` → zsh's ZLE cannot look up `cub1`/`el`/`ed` terminfo caps →
backward-delete-char emits just `0x20` instead of `0x08 0x20 0x08`, kill-line wipes the prompt row, and ncurses
TUIs refuse to start.

**Right** — **an explicit `TERM` in the env of every PTY child**, set where the shell spec is resolved
(`crates/roost-worker/src/host/shell_spec_resolver.rs`), together with `LANG`/`LC_ALL` fallbacks so the
same SSH-bootstrapped env doesn't surface a locale bug next.

**Guard** — `crates/roost-worker/tests/shell_spec_resolution.rs` —
`a_resolved_spec_sets_the_terminal_and_locale_variables_explicitly`.

### An app shortcut swallows a control byte

**Symptom** — "Ctrl-F / a control key stops reaching the PTY after adding an app shortcut — `cat -v` shows the byte missing while the app UI opens instead"

**Wrong** — bind the chord anyway and try to `stopPropagation` selectively, or "fix" the test's expectation.

**Right** — **a capture-phase document handler on the pane runs BEFORE the key can be encoded, so it must never
claim a bare Ctrl+letter.** The terminal's own textarea handler is what `preventDefault`s a consumed control
byte, and every document-level BUBBLE listener already respects that — capture-phase bypasses it entirely.
Terminal-scoped chords use ⌘+key (macOS, never a PTY byte) or Ctrl+SHIFT+key (the gnome-terminal shape); find is
`⌘F / Ctrl+⇧F` for exactly this reason, resolved centrally in `crates/roost-web/src/platform/browser_platform.rs`. Before
adding one, check it is not a readline/TUI binding.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A global key router claims bare ↑/↓/⏎ on routes that have no cursor

**Symptom** — "I can't scroll" on a TV remote / D-pad — including on `/pair`; also "OK does nothing on
Request approval / Pair / Approve / Deny"

**Wrong** — a `window` CAPTURE-phase handler that `preventDefault()`s bare `ArrowUp`/`ArrowDown`/`Enter`
whenever no modal is open and no terminal deck is mounted, then routes them to a list cursor. Off the sidebar
that cursor's id list is EMPTY, so the keys move nothing while still cancelling the browser's work. Also wrong:
"fixing" it per-route, or gating on the route path — the predicate is whether a cursor target exists, not where
you are.

**Right** — **claim a key only when there is something to move.** `crates/roost-web/src/keyboard_shortcuts.rs`'s
arrow/⏎ branch bails before `preventDefault()` on `!hasCursorTargets()` (arrows) and `cursorSessionId() === null`
(⏎), both from `crates/roost-client-core/src/store/sidebar/cursor.rs`. Two distinct defaults are at stake and both are invisible
until they are gone: arrows are the DOCUMENT'S native scroll, and keydown `preventDefault()` cancels a focused
`<button>`'s click activation — so ⏎ on a real button dies silently with no console trace. Generalizable rule:
a capture-phase router must prove it will act before it cancels.

**Guard** — `crates/roost-web/tests/keyboard_shortcuts.rs` — `arrows_stay_native_scroll_when_no_cursor_rows_exist`
and `enter_stays_a_focused_buttons_activation_until_a_row_is_highlighted`.

### The first D-pad press does nothing because `<body>` counts as the origin

**Symptom** — a TV remote on a page that fits the screen (the unpaired pairing gate, any short route): ↓ never
reaches **Request approval** or any other control, focus stays on `<body>`, and no `dpad.nav` line is emitted.

**Wrong** — using `document.activeElement.getBoundingClientRect()` as the travel origin whenever it has size.
After load or a route change focus sits on `<body>`, whose box contains every control, so no candidate is ever
"beyond" it in any direction and the search returns nothing. Also wrong: autofocusing a button per page to paper
over it — every other route keeps the dead first press.

**Right** — `<body>` is never an origin. `crates/roost-web/src/input_nav/spatial.rs` `pick_target` treats
`active === document.body` as no origin and lands on the topmost-leftmost control, as its doc comment always
intended.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A terminal domain reset is treated as the input fence

**Symptom** — "Input may have been partially sent; it was not retried" after a terminal domain reset /
composer freezes ~10 s then reports ambiguous

**Wrong** — fencing terminal input on the terminal `domainGeneration`, so a `domain_overflow` /
`aggregate_overflow` / recovery reset on a LIVE socket settles every in-flight batch as ambiguous and then
discards the coordinator's late result; and returning silently from coord's terminal command gate when an
`input` command is refused, which leaves the browser waiting out `INPUT_RESULT_TIMEOUT_MS`.

**Right** — **the socket is the input fence.** Input results ride the CONTROL lane
(`crates/roost-coord/src/sync_ws/control_frames.rs` stamps `domain = UNSPECIFIED, domainGeneration = 0`), which no
domain reset touches, so a started batch keeps its 10 s deadline and settles from the real result;
the client core's terminal input router (`crates/roost-client-core/src/terminal/input/`) correlates on `(socketId, sessionId, inputSeq)` plus the
generation coord echoes from the command, and `handleGeneration` only settles pendings when the SOCKET changed
(an unsent batch under the closing generation is `rejected`, never ambiguous). `resetSequence()` runs only on a
socket change, because a surviving pending must not share an `inputSeq` with a new batch. Coord's
`sync-ws-v2-commands.ts` answers every refused `input` with an `inputRejected` carrying the command's own
`domainGeneration` and the refusal reason — nothing reached a worker, so `rejected` is the truthful
classification and the composer restores the draft instead of claiming possible loss.

**Guard** — `crates/roost-client-core/tests/terminal_input_route_loss.rs` —
`a_terminal_domain_reset_on_the_live_socket_keeps_the_route_epoch`.

### Delayed old-route input crosses a direct-promotion fence

**Symptom** — "a key sent on Sync appears after WebRTC became active / an old-route input reaches the PTY after
direct promotion".

**Wrong** — treat browser no-replay, a closed old socket, or a new renderer route as the input fence. A
coordinator→worker `DInputRequest` already in flight can arrive after the browser has promoted a direct route.

**Right** — the worker owns the fence: `TerminalInputRouteOwner` issues the actor/session route epoch, and
the worker's input write path (`crates/roost-worker/src/terminal_input/`) rechecks live route authority after keeper
admission immediately before `beginInput`. A stale epoch returns `terminal input route changed`; it never writes
the PTY.

**Guard** — `crates/roost-worker/tests/terminal_input_write.rs` — `live_authority_is_rechecked_after_the_lane_is_granted`.

### The pane reads Coordinator and every keystroke is refused as "terminal input route changed"

**Symptom** — after a WebRTC route fell back to Sync, a Sync reconnect (laptop wake, network change,
coordinator restart, a stale link replaced on tab resume) leaves the terminal reading Coordinator while
nothing typed reaches the PTY: every batch settles `rejected: terminal input route changed`, the input
lane reads `sending`, and it lasts until some later promotion moves the route. After a reload in the same
tab the new document's keystrokes are refused the same way until it promotes — for the worker's whole
60 s route tombstone when it cannot peer.

**Wrong** — treating epoch-less Sync input as always admissible, and keying the route epoch by the full
token. The worker fences a session's input to the device/tab route its last claim came over, connection
included, and refuses every epoch-less batch while that route or its tombstone exists
(`roost-worker` `route_owner::allows_legacy_input`). An epoch keyed by terminal-domain generation is
forgotten by a domain reset on the same socket, one claimed over a socket that closed fences nothing on
the next, and a lane that blocked had no path back to sending.

**Right** — the epoch belongs to the CONNECTION (`TerminalToken::same_connection`, v2
`terminalInputConnectionKey`): a domain reset keeps it and a claim answered after one still settles. A
Sync close blocks every sending lane whose epoch it claimed (`InputRouter::block_routes_on_sync_socket`,
v2 `retireTerminalInputRouteState`); a Sync batch the worker refuses as route-changed while no direct
route serves the session blocks the lane too (`block_moved_sync_route`); and a keystroke into a blocked
lane with no direct route claims Sync back and waits in that claim's hold instead of being refused
(`handle_input::reclaim_lost_route`).

**Guard** — `crates/roost-client-core/tests/terminal_input_route_loss.rs` (all three failed before the
fix).

### node-datachannel `sendMessageBinary(false)` is accepted buffered delivery

**Symptom** — "a direct terminal fragment duplicates after WebRTC backpressure / a false native send result
resends a control, cell, or history fragment".

**Wrong** — interpret `node-datachannel` `sendMessageBinary(...) === false` as refusal and retry the fragment.
The native channel accepted it into its buffer, so retry duplicates protocol bytes.

**Right** — `TerminalPeerPacketPort` commits the fragment exactly once; `false` marks the lane
`backpressured` and waits for its low-water callback. Queue refusal happens before the native call; a native
throw retires the peer.

**Guard** — `crates/roost-worker/tests/terminal_peer_packet_port.rs` —
`commits_a_native_false_return_once_without_retrying_its_accepted_fragment`.

### Every refused microphone reports "did not respond in time"

**Symptom** — "the mic says it did not respond" / "the voice input times out on every attempt" / a refusal —
a permission refusal, a missing device, a policy block — is reported with the DEADLINE's wording rather than
with its own cause. The refusal is therefore indistinguishable from a device that was granted and never
answered, and every refused microphone costs the full deadline before the operator is told anything at all.
The tell is that the timeout fires even where no device could plausibly have answered.

**Wrong** — attaching only a success handler to the handshake promise. A promise settles by rejection as well
as by fulfilment, so with nothing on the reject arm the race is never settled by the refusal; the deadline
wins by default and its timeout becomes the answer to every failure. Raising the deadline does not help — it
lengthens the wait for the cases that already work and still mislabels the ones that do not.

**Right** — **every exit from a promise must settle the race, and a rejection carries the reason the operator
needs.** Attach both arms, and let the reject arm publish the refusal's own cause so the timeout wording is
reserved for the case that genuinely means "nothing came back". This is not the diagnostic-sink entry in "Browser
platform reality": a throwing sink corrupts the observation of a path that works, whereas this is a path that
never reaches its own handler at all, so no observer of the failure can see the real reason.

**Guard** — `crates/roost-web/src/voice/handshake/tests.rs` asserts that a rejected handshake settles the race
with the refusal's own cause rather than the deadline's.

---

## Worker, keeper and host

### Pane close races the worker reading the kill

**Symptom** — "pane ✕ click does nothing"

**Wrong** — send kill + immediately `conn.close()` (the browser close frame races the worker reading kill).

**Right** — the worker's kill path synchronously acks with a `closed` control message
(`crates/roost-worker/src/browser_commands/session_lifecycle.rs`); the browser waits for that ack before tearing down.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A `u32::MAX` pid is `kill(-1)`, and one test call SIGINTs the whole user session

**Symptom** — "every omp session died at once / all the agent sessions vanished and nothing crashed", with
`"Session exit recorded" … "reason":"sigint","kind":"signal"` in every track's log in the same second, and the
journal showing `systemd[...user manager]: Received SIGINT from PID N (kill)` immediately followed by
`Activating special unit Exit the Session`.

**Wrong** — reach for "a pid that cannot exist" in a test or a shutdown path by passing a sentinel like `u32::MAX`
into a `u32` pid API, assuming a pid that names no process also cannot name a group. It can: `kill` parses its
operand into a **signed** `pid_t`, so `4294967295` wraps to `-1` (every process the user may signal) and `0` is the
caller's own process group. The unit test in `crates/roost-cli/src/dev/signal.rs` sent exactly that, so every
`cargo test -p roost-cli` SIGINTed the systemd user manager, the Roost worker hosting the terminals, and every
agent session the user owned. Measured on this host: four such shutdowns at 02:25:15, 03:08:02, 08:31:30 and
09:35:13 on 2026-09-27, each 100–233 s after a `cargo test -p roost-cli` — the time to reach the lib unit tests.
Reproduced inside `unshare -Urpf --mount-proc` with a SIGINT-default sentinel: the old test killed the sentinel
AND still FAILED, because `kill(-1)` succeeds, so `send` reported `Ok(())` instead of `Refused`. The test binary
usually outlives its own `kill(-1)`, which is why this presented as an intermittent session wipe rather than an
obvious test failure — and why a green-looking run on an unpatched tree proves nothing.

**Right** — refuse at the boundary, before anything is spawned: `dev::signal::send` returns
`SignalError::NotASingleProcess { pid }` when `pid == 0 || i32::try_from(pid).is_err()`
(`crates/roost-cli/src/dev/signal.rs`). Mind where the signed parse actually happens: this module shells out to
the `kill` PROGRAM because the crate forbids `unsafe` and `Child::kill` is SIGKILL-only, so the wrap lives in
another binary and only a guard at the call site can prevent it. The one production caller needed no change —
`supervisor.rs`'s `signal_the_live` already had a catch-all `Err(failure)` arm, and its pids are all `Child::id()`.

**Guard** — `crates/roost-cli/src/dev/signal.rs` unit tests:
`a_pid_kill_would_read_as_a_group_or_as_everyone_is_refused_before_anything_is_sent` (0, `u32::MAX` and
`1 << 31` never reach the program) and `a_pid_that_no_longer_exists_is_refused_rather_than_reported_as_sent`
(Linux-only; uses `pid_max` from `/proc`, which Linux never allocates — macOS has no `/proc` and would need a
different nonexistent pid). The keeper encodes the same invariant independently in
`crates/roost-keeper/src/process_reap.rs`: its deliberate group signal is
guarded by `leader > 1`, which structurally excludes `-1`. The other `kill` shell-outs are safe for a reason
worth stating so nobody "fixes" them: `status/service_probe.rs` signals `child.id()` of a probe it spawned, and
`crates/roost-cli/tests/dev_fan_out.rs` signals `std::process::id()`.

### A worker throttled by its own cgroup looks healthy

**Symptom** — "a worker shows offline/down in the SPA while `systemctl --user status roost-worker` says active (running) and the host has GBs free / worker log silent for minutes then `link_stale_no_downstream` + `listChannels timed out` + `heartbeat beat failed [unavailable] HTTP 502` / coord `worker-ws close`→`open` gap of ~361s / v3: `the coordinator link has gone silent; forcing it closed` with `silent_ms` far past the stale threshold + `stray_reap_list_failed … did not answer a spawn within 5s` + `cgroup_memory_high_exceeded`, keeper in `D` at `mem_cgroup_handle_over_high`"

**Wrong** — chase the 502 into the front-door proxy, restart the worker, or read the SPA's host metrics and conclude
the box is healthy — the host sampler reads host-wide `/proc/meminfo`, so a unit strangled
by its own `MemoryHigh` publishes "8.7 GB of 33.6 GB used" while every allocation in its cgroup is throttled;
equally wrong: adding `MemoryMax` (every PTY session shares this cgroup, so a hard cap plus `Restart=always`
turns one fat session into a fleet-wide session wipe). Measured on a live host: cgroup
`memory.current=3401814016` vs `memory.high=3221225472`, `memory.events high` climbing ~150k/min, worker MainPID
in `D (disk sleep)`, 6 PTY sessions = 2.9 GB in the SAME cgroup, `SwapFree 172 kB` so reclaim had nowhere to go.

**Right** — **three layers, all required.** (1) `MemoryHigh` must scale with the host:
`crates/roost-cli/src/services/memory_limits.rs`
(`ResourceLimits::worker`) is 60% of MemTotal, floor 3G, **no cap**, absolute (systemd only takes % from v240);
`TasksMax=4096`, not 512. A cap is the same defect as a flat value: v3 shipped a 3G cap, and on a 62 GiB host with no
swap two agent sessions plus a release build froze the keeper in `mem_cgroup_handle_over_high` and starved the
coordinator link (`Stale { silent: 1040s }`, `did not answer a spawn within 5s`) while v2's 37G unit served the same
box. The live value can sit in a hand-written
`~/.config/systemd/user/roost-worker.service.d/limits.conf` drop-in that OUTRANKS the deployed unit body — check
the drop-in before editing the unit. (2) A dial that never fires `ws.onopen` is NOT an auth rejection: coord
answers a bad JWT with an HTTP 401 upgrade, indistinguishable from a timeout or a proxy 502 at the dialing
client, so throttle-induced dials used to arm the auth-reject backoff cap and turn a ~20s stall into ~6 min.
`crates/roost-worker/src/backoff.rs` keys escalation on whether the link ever opened; the log is
`reconnect_backoff_escalated`, never `auth_rejection_escalated`. (3) The heartbeat metrics
(`crates/roost-worker/src/runtime/heartbeat_metrics.rs`) emit
`cgroup_memory_high_exceeded`/`_cleared` so the next occurrence is one grep, not a guess.

**Guard** — `memory_limits.rs` unit tests
(`the_worker_soft_ceiling_*`), `crates/roost-worker/tests/backoff_policy.rs`,
`crates/roost-worker/tests/worker_reconnect_ladder.rs`, `crates/roost-worker/tests/cgroup_throttle_health.rs`.

### A worker reconnects but respawns every terminal

**Symptom** — "worker WebSocket opens and heartbeats are fresh, but every workspace remains unavailable /
worker logs `resume_failed` with `[unauthenticated] authentication required`, followed by a burst of
`respawn_if_missing_spawning` instead of keeper adoption"

**Wrong** — treat a successful worker WebSocket as terminal recovery, let the coordinator's delayed
`respawn-if-missing` fallback recreate every database row, or grant workers browser/session mutation authority.
The fallback preserves sidebar rows but loses the prior subprocess and terminal context.

**Right** — `SessionsList` admits a worker principal only when the request names that exact worker fingerprint,
requests only `status=open`, and carries no browser sync-snapshot ID. Boot reconciliation can then advance the
channel counter and adopt keeper survivors before the coordinator fallback runs; every other session RPC
remains account-device-only.

**Guard** — none — the v2 guards `apps/coord/tests/workers/worker-session-list-auth.test.ts` and
`apps/worker/tests/boot/boot-reconcile-admission.test.ts` left with the TypeScript tree.

### A live viewport change rebuilds the terminal core

**Symptom** — "tab switch stalls for seconds and reloads scrollback / unchanged
TUI chrome disappears after resize / switch cost depends on retained output"

**Wrong** — rebuild a fresh emulator from a bounded raw-byte ring for every
viewer claim or resize. The ring may no longer contain the bytes that produced
the current screen, so replay legitimately forgets static cells; it also
re-instantiates WASM and reparses history on the worker's event loop.

**Right** — the coordinator computes one SCD geometry and addresses it with a
new stream ID. The keeper resize ACK is the ordered boundary between old-size
and new-size PTY bytes. At that boundary the worker calls
the terminal core's resize on the existing core, resets only the cell
emission baseline/epoch, and emits one viewport-only full. Primary and
alternate grids, modes, links and representable scrollback stay in memory.
Keeper-history replay is reserved for genuine worker adoption when no live core
exists; an unprovable resize boundary fails closed.

**Guard** — `crates/roost-worker/tests/terminal_stream_state.rs` —
`shrink_and_grow_resize_the_same_core_at_the_keeper_boundary`;
`crates/roost-coord/tests/terminal_view_geometry.rs`.

### Quoting a systemd path directive because quoting is "safer"

**Symptom** — "`roost push` stages the release and then fails activation: `Unit roost-coord.service has a bad unit file setting` / `WorkingDirectory="/home/user/roost": path is not absolute`, the push rolls back, and the whole fleet stays pinned at the older commit while every Linux coordinator/worker deploy fails identically / or the unit starts clean and writes NO logs — `main.out.log` never grows and the journal carries `Failed to parse output specifier`"

**Wrong** — treat systemd quoting as universal and pipe every dynamic value through
`systemd_quote()` in the v2 shell installers. It is tempting because
the launchd branch of the SAME function must XML-escape everything it interpolates into the plist, and because
quoting is genuine systemd syntax where it applies: `ExecStart=` is a command line and `Environment=` is a
key=value list, so both really do accept (and for a path with a space, really do need) double quotes. One
uniform escape helper for every interpolated value therefore looks like the conservative choice — and it is the
one that bricks the unit. The two failure modes do not even look alike: `WorkingDirectory=` is FATAL and loud,
while a quoted `StandardOutput=`/`StandardError=` specifier is discarded SILENTLY, so the service comes up
"healthy" and its logs simply never exist.

**Right** — **quoting is per-directive, not per-file.** `ExecStart=` and `Environment=` are parsed as quoted
command lines; `WorkingDirectory=`, `StandardOutput=` and `StandardError=` take the RAW value — the quotes
become part of the path, so `WorkingDirectory=` fails `path is not absolute` and the unit refuses to start, and
the output specifier fails to parse and is dropped with no error and no log file. The unit writer
(`crates/roost-cli/src/services/systemd_syntax.rs`) rejects a value containing newline, CR or `"` (the characters
that would let a value forge a directive line, which is the only thing the quoting bought), doubles `%` so the
value can never be read as a systemd specifier, and emits a path directive raw; `ExecStart=` and `Environment=`
keep their quoted forms (which are legal there). Second-order lesson:
writing a unit file is not activating a service — activation must be proven by systemd actually STARTING the
unit, which is exactly what caught this. The push never saw a healthy coordinator at the
expected SHA, took its `rollback-prior` branch and restored the previous release, so the fleet sat on an old
commit instead of "succeeding" onto a dead one; a deploy path that trusted "the unit file was written" would
have reported success against a coordinator that was never running.

**Guard** — `crates/roost-cli/tests/services_definition_text.rs` — renders both roles' units from one spec and
asserts the path directives are emitted RAW (`WorkingDirectory=` and `StandardOutput=` carry no quotes) while
`ExecStart=` and `Environment=` are quoted, then hands the coordinator's unit to `systemd-analyze --user verify`
where that tool exists, so a re-quoted path directive fails in CI rather than on the first `roost push`.

### A fresh macOS account has no LaunchAgents directory

**Symptom** — "`roost join` reaches `activate staged com.roost.worker-v2` and
fails `mktemp: mkstemp failed on ~/Library/LaunchAgents/com.roost.worker-v2.plist.new.*:
No such file or directory`; no worker service is installed."

**Wrong** — create only worker data and log directories before atomically staging
the plist. `mktemp "${PLIST}.new.XXXXXX"` stages beside the target, so the first
install fails whenever the plist parent has not already been created.

**Right** — every macOS `write_plist` creates `dirname "$PLIST"` together with
its data and log directories before staging. The coordinator and worker installers
share that first-install invariant.

**Guard** — `crates/roost-cli/tests/services_install_idempotence.rs` — "the directories a service needs are
created before the first definition" resolves a spec against a tree with no `LaunchAgents` directory and proves
`ensure_service_directories` creates the data directory, the log directory and the definition's own parent before
anything is staged into it.

### A remote deploy hands the target the deploying box's identity

**Symptom** — "the machine I deployed to came up with another machine's name" — the coordinator lists two
workers under one label, and the deployed machine's real identity is missing from the fleet view.

**Wrong** — resolve every deploy variable through one uniform order,
`invocationValue ?? installedEnv[key] ?? process.env[key]`. It reads as an obvious convenience — the ambient
fallback is what lets an operator export `ROOST_COORDINATOR_URL` once and deploy the whole fleet without
repeating it. But `ROOST_WORKER_LABEL` and `ROOST_REACHABLE_ADDR` do not describe the fleet, they name ONE
machine, and the process holding that ambient env is the box running `roost deploy`, not the target. Deploying
to a host with no installed service definition therefore installs the DEPLOYING box's label and reachable
address on it; the target registers under a name that already belongs to another worker, and because
`reachable_addr` is what the SPA builds a machine's address from, the wrong machine
is addressable under that name. Nothing warns: both values resolved, so the deploy looks complete.

**Right** — the resolution order is per-key, from an explicit classification, not per-call. The identity keys
live in one static table (`crates/roost-cli/src/services/service_environment.rs`) that also names the deploy flag
that supplies each, and resolution takes an explicit
`target: "self" | "remote"` saying whose machine the ambient env describes. For `"remote"` an identity key
resolves only from the invocation flag (`--label`, `--reachable-addr`) or the target's own installed plist /
unit; for `"self"` the ambient env is the target's own and stays valid. Unresolvable is not an error by itself —
the worker derives its hostname and tailnet name, which is the documented fresh-target path — but
`resolveRemoteDeployIdentityEnv` REFUSES the deploy (`failDeploy(6, …)`) when the deploying shell exports that
key and nothing else resolved it, because that is exactly the ambiguity that mislabels a fleet. Fleet-wide keys
(`ROOST_COORDINATOR_URL`, `ROOST_BOOTSTRAP_TOKEN`, diag flags) keep the ambient fallback.

**Guard** — `crates/roost-cli/tests/deploy_remote_identity.rs` —
`an_ambient_identity_export_refuses_and_names_the_flag`, `an_identity_key_never_resolves_from_the_deploying_shell`,
`an_unresolvable_identity_with_nothing_exported_is_allowed`.

### Roost cannot upgrade the integration asset Roost installed

**Symptom** — "agent status stopped reporting after an upgrade" — the worker logs
`refusing to overwrite non-Roost extension: ~/.omp/agent/extensions/roost-omp-agent-state.ts` (or
`agent integration target ownership changed before commit: …`) on every boot, the installed asset stays at the
old version forever, and session status silently degrades to screen detection because no integration report
ever arrives.

**Wrong** — recognize the `ROOST_INTEGRATION_ID=<runtime>` ownership marker only in the file's first lines
(`content.split(/\r?\n/, 8)`, or the contiguous leading `//` header). It reads as a tightening — a marker in
the header is a marker Roost wrote — but the installed form of an asset is NOT the source form:
`standalone-integration.ts` splices the shared `report-transport.ts` module in, and an earlier release spliced
it ABOVE the integration's own header, so a deployed asset carries its marker on **line 106**, under ~100 lines
of transport code (line 14 onward is `import net from "node:net";`, so it is not a comment header either). Roost's
own correctly-marked files therefore read as somebody else's, and both the planning refusal and the commit-time
guard fail closed against the installer itself. Any positional window is the same bug with a bigger constant:
the prefix is another module's entire source, so it has no bound to pin. The second half of the same failure is
planning the asset set with `Promise.all`: ONE refusal rejects the batch, so a stale unowned `.pi` asset blocks
the `.omp` asset from ever being written even with the omp path free.

**Right** — **depth is not evidence of authorship; the token is.** `hasIntegrationOwnership` accepts the marker
as its own whitespace-delimited token on ANY `//` comment line in the file (fast-path bail-out when the marker
substring is absent), and nothing else loosens: a non-comment line that merely mentions the marker, a near-miss
token (`ROOST_INTEGRATION_ID=omp-reference` for `…=omp`), and a file with no marker are all still refused. Both
call sites share that one predicate, so planning and `assertIntegrationTargetUnchanged` cannot disagree. Each
target is then planned through `capturePlanFailure`, which turns one target's refusal into a reported failure
(`integration_install_failed` with `runtime`/`path`) instead of a throw: the remaining assets still install and
`installAgentIntegrations` returns `{installed, failed}`. Target-collision proof moved AHEAD of planning and runs
over every candidate, so a dropped target cannot relax it. Transactional failures (directory alias, commit-time
change) still abort the whole pass with zero mutation — only per-target refusals are isolated.

**Guard** — `crates/roost-worker/tests/agent_status_integration_ownership.rs` —
`adopts_and_overwrites_an_installed_asset_marked_below_the_file_head`,
`the_marker_is_recognized_in_any_comment_line_and_nowhere_else`,
`the_commit_guard_passes_a_target_marked_below_the_file_head`.

### A one-shot deploy flag stops at the installer process

**Symptom** — "`roost deploy <host> --force-live` printed the destructive-authorization banner, staged, wrote
the plist — and the worker then EXITED with `keeper_survivor_identity_unproven` / `keeper endpoint is held by a
process that did not prove keeper identity; stop that process, then restart the worker`", so the operator has
to stop the service, kill the legacy keeper and delete the mux socket by hand — which is the exact work the
flag exists to avoid.

**Wrong** — treat "the flag is in the composed install environment" as "the flag reached the worker". A POSIX
deploy ran `<composed env> bash install.sh write-plist` (the v2 installer) over ssh, so every variable in
that prefix is real — in the INSTALLER's process. `install.sh` then writes an explicit key set into
`EnvironmentVariables` / `Environment=`, and a key absent from that set dies with the installer's shell: the
worker launchd/systemd starts never sees it. Nothing warns, because both ends are individually correct — the
CLI composed the value (`deploy.ts`, `deploy-macos.ts`, `deploy-local.ts` all pass
`ROOST_KEEPER_FORCE_LIVE_RETIRE`), `config.ts` parses it, `boot-keeper.ts` branches on it, and the deploy log
line `>> reused from existing plist on <host>: …` even proves the environment merge worked. The worker simply
booted on the non-force path and refused, which reads as "the flag was ignored" rather than "the flag was
never installed".

**Right** — **a value only reaches the service if the service DEFINITION carries it.** The definition writer
emits `ROOST_KEEPER_FORCE_LIVE_RETIRE` (`crates/roost-platform/src/worker_service_env.rs`) only when the
invocation armed it, and never reads it back off an installed definition — an invocation not given the flag
simply omits the key. Writing a destructive authorization into a definition then creates the opposite hazard,
a flag that re-authorizes discarding live PTYs on every later restart, so it is one-shot on BOTH sides: the
activation that reads it spends it after admission and before any link work, and the next deploy strips an
installed value anyway. The force branch also names what it destroys BEFORE requesting the shutdown — the
keeper's binding channel ids and spawning channels, `null` when the survivor could not enumerate them, which is
why the authorization was needed at all.

**Guard** — `crates/roost-cli/tests/services_definition_text.rs` — "a one-shot grant is never carried into a
definition" resolves a worker spec with both grants set and pins that neither reaches the rendered definition,
while a caller that arms one through `ServiceSpec::with_setting` gets it, so the flag has to be installed
deliberately for one deploy; `crates/roost-worker/tests/boot_keeper.rs` — an authorized boot logs the discarded
bindings before the retirement, spends its own authorization, and the same survivor still yields
`KEEPER_IDENTITY_UNPROVEN` without the flag; `crates/roost-worker/tests/worker_retire_authorization.rs` —
`a_force_live_retire_authorisation_is_spent_once_after_admission`.

### Repairing a dead worker demands that the dead worker be running

**Symptom** — "`DeployFailure: existing macOS worker mike-m5-air has a stale keeper update proof; start the
worker on <host> so it can prove admission`" — `roost deploy` refuses the very host it exists to repair, and
booting the wedged service out (the only way to clear a wedged keeper) makes it refuse harder with
`prior macOS worker lifecycle did not round-trip`; the operator ends up hand-writing the plist and
`launchctl bootstrap`ing it.

**Wrong** — gate staging on the coordinator's registry row plus one `test -e` against the service definition.
Both halves look like the conservative choice and both describe the wrong machine. A stale row is a statement
about what the COORDINATOR last heard, not about what the host is running: a worker that died an hour ago and a
healthy worker behind a broken tailnet hop produce the identical row, so refusing on staleness refuses exactly
the machine that needs repair. The `test -e` then proves only that a FILE exists — an installed plist or unit
says nothing about whether launchd/systemd ever loaded it, whether a worker process is alive, or whether a
keeper is still holding PTYs. The remedy the refusal prints ("start the worker so it can prove admission") is
unreachable for a host that is down, and impossible for the build that cannot report a keeper runtime at all.

**Right** — **the registry supplies the refusal text; the target supplies the evidence that decides whether it
stands.** The deploy admission (`crates/roost-cli/src/deploy/admission.rs`) runs ONE probe on the host and stages only on positive proof
of emptiness: the service definition is absent, or the service manager itself answered AND reports the worker
not running AND no keeper process is parenting a channel process. Every unknown fails closed, because an
unreachable service manager reads exactly like a stopped one in its own output: darwin corroborates a failing
`launchctl print` with a `launchctl print-disabled` domain query (an unloaded job and an unreachable launchd
share an exit code), Linux requires `systemctl show` — which exits 0 even for a unit it has never heard of — to
exit 0, and a host whose PATH has no `pgrep` cannot prove no keeper is alive and is refused. A keeper SOCKET
FILE is deliberately not evidence: it outlives the keeper that created it, so it can neither prove nor disprove
anything the process counts do not. Live PTYs keep every refusal they had — a running worker, or a keeper
holding channels, still refuses — and a permitted install prints one line naming the evidence that allowed it.

**Guard** — `crates/roost-cli/tests/deploy_keeper_admission.rs` — `a_stale_row_over_a_target_running_nothing_stages`,
`a_stale_row_over_a_running_worker_still_refuses`, `a_keeper_holding_channels_refuses_even_with_the_worker_stopped`,
`an_unknown_never_stages`, `the_darwin_probe_distinguishes_an_unreachable_launchd`,
`a_keeper_socket_file_decides_nothing`, `a_permitted_staging_says_what_allowed_it`.

### A rollback proof no release can satisfy wedges every later deploy

**Symptom** — "`prior macOS worker lifecycle did not round-trip; journal retained`" alongside
`Could not find service "com.roost.worker-v2" in domain for user gui: 501` / `RoostLaunchdLoaded=no` —
after ONE failed activation, every later deploy to that host dies in journal recovery, and the operator
escapes only by moving the retained journal aside by hand and activating the staged release directly.

**Wrong** — assume a retained journal is always recoverable. The rollback restores the prior release's
plist (or unit) and then demands that release return to its recorded lifecycle with an advanced pid — but
the new release already migrated the worker's durable session-event store forward, so the prior release
dies at boot with `SessionEventStoreFatalError: session event store schema mismatch`. The proof can never
pass, the journal is retained by design, and each later deploy replays the same doomed rollback. Deleting
the journal on any failed proof is equally wrong: a rollback that was never started, or is transiently
failing, must keep it.

**Right** — journal the fact that decides it. The durable store's schema version is captured at prepare
(`priorDurableStateVersion`) and re-read at the rollback checkpoint (`targetDurableStateVersion`), from a
64-byte SQLite header read that takes no lock — macOS journal schema v3, Linux journal schema 5, both
parsing their predecessor with the versions unobserved. When the rollback actually started the prior
release AND the journal proves the store moved past it, recovery resolves to the terminal
`roll-forward-required` outcome: it prints why, keeps the staged release, and clears the journal so the
next deploy runs. Un-started, un-migrated, and previous-schema journals still retain and retry.

**Guard** — none — the v2 guard `apps/roost-cli/tests/deploy-roll-forward-recovery.test.ts` left with the
TypeScript tree.

### Settlement retires the prior release with a command only a worktree accepts

**Symptom** — "deploy exit 5: cannot retire prior worker release …: fatal: '…' is not a working tree" — the
new release is already installed and serving when the deploy reports failure.

**Wrong** — assume a release directory is a git worktree because the developer's own checkout is one.
Every release a real host has was staged by rsync, so `git worktree remove` fails it, and it fails at
SETTLEMENT — after the service definition points at the new release — which reads as a failed deploy of a
worker that is actually running the new code.

**Right** — ask git whether the path is a registered worktree (`git worktree list --porcelain`) and
otherwise remove the directory outright. The symlink refusal and the release-root confinement proof that
already guard this path are what make the plain removal safe; keep both ahead of it.

**Guard** — `crates/roost-cli/tests/deploy_installed_release.rs` —
`a_staged_prior_release_is_retired_without_being_a_worktree`, `retirement_is_confined_to_the_release_root`.

### A retired release's dist leaves every page a 404 while the API still answers

**Symptom** — "every page is 404 / `not found` in the browser but the API and terminals still work / the UI
died after a deploy or a reboot".

**Wrong** — trust the `ROOST_WEB_DIST_PATH` a deploy stamped into the installed service. It points INTO a
release directory that a later settlement deletes
(the v2 deploy environment writer recorded why the value is never carried forward), and
the v2 coordinator's `createSpaResponder` then picked no source at all: its SPA arm answers the
bare `not found` 404 for `/` and every deep link, which reads like an edge, DNS or certificate fault
because the RPC surface on the same listener is untouched. Chasing the front door here costs the outage.

**Right** — the SPA source is startup-visible state, not something to infer from a 404.
`createSpaResponder` reports the build it chose (`source: "disk" | "embedded" | "none"`, derived from that one
choice and never re-probed), the v2 coordinator logged `spa_source_missing` once when that was `"none"` and
keeps serving — worker links and keeper state outlive a browser build that went away. `roost status` must not
repeat the inference: the CLI cannot read a released install's embedded manifest, so it HEADs the
coordinator's own root and reports that answer next to the stamped path. A source install points
`ROOST_WEB_DIST_PATH` at the source tree's own web dist, which no settlement deletes; a released install
re-stamps it per deploy.
These three make the state visible; what stops the commonest way INTO it is the next entry, "An installer
inherits a sibling service's dist path from the shell that ran it".

**Guard** — `crates/roost-cli/tests/status_output_shape.rs` — `an_unprobed_spa_is_neither_a_pass_nor_a_failure`
pins the `roost status` half; the startup-report half has none — the v2 guards
`apps/coord/tests/spa-source-startup.test.ts` and `packages/host/tests/spa.test.ts` left with the TypeScript tree.

### An installer inherits a sibling service's dist path from the shell that ran it

**Symptom** — a coordinator unit whose `ROOST_WEB_DIST_PATH` points inside
`RoostWorkerV2/service/releases/worker/…` (or a worker unit pointing into the coordinator's releases), so the
UI dies the next time the OTHER service deploys and retires that release.

**Wrong** — read `ROOST_WEB_DIST_PATH` straight out of the environment in `write_plist`/`write_unit`:
`web_dist="${ROOST_WEB_DIST_PATH:-<source tree>/web/dist}"`. The variable arrives from whatever ran the
installer, and the programmatic callers pass the ambient environment through —
the v2 quickstart's `runInherit` spawned with `{ ...process.env, ...env }`, and
the env it merges (`coordinatorEnvironmentForQuickstart`) names a bind, a public URL and
`ROOST_SKIP_ENV_LOCAL`, nothing about the dist. So a dist exported for a DIFFERENT service silently lands in
this service's definition. The CLI's carry-forward already strips the key
(the v2 worker-environment carry-forward) — the hole was the shell installers, which are also
what `roost status` and GETTING_STARTED tell an operator to run by hand. Do not fix this by scrubbing the key
at each caller: the installers are the single owner of what their own unit may name.

**Right** — `resolve_web_dist()` in both installers honors an explicit value only when it resolves inside that
install's own root, and otherwise warns on stderr and stamps the source tree's own dist. Every legitimate
caller satisfies it: the coordinator push sets `ROOST_REPO_ROOT` to the release it staged, and the
worker's remote activation runs the release's own installer with that release's own dist, so
the script-derived root already contains it. The worker installer takes no `ROOST_REPO_ROOT` override at all,
which is why its root cannot be spoofed. The check never fails an
install — a compiled install serves its embedded build regardless.

**Guard** — none — the v2 guard `apps/roost-cli/tests/coord-installer.test.ts` left with the TypeScript tree.

### Moving a keeper-imported file makes every live keeper unadoptable

**Symptom** — `Expected: "worker-only-safe" Received: "incompatible-with-live-sessions"`.

**Wrong** — move `session-scrollback-ring.ts` (or change the digest algorithm). Bun's minified identifier assignment includes import-specifier text, so a path-only move of a closure file changes the implementation digest even when its code is byte-identical.

**Right** — keep every file in the keeper bundle closure at its existing path and import specifier, then check `buildKeeperImplementationDigest()` after any move. Changing the digest algorithm would reject the implementation identity already embedded in every live keeper.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

---

## Transport and connection lifecycle

### Half-open WS survives a coord restart and never closes

**Symptom** — "new terminal → [failed_precondition] worker … not connected / worker log silent (no stream_error) for hours / heartbeats fine, lsof shows ESTABLISHED to :4102"

**Wrong** — restart the worker by hand / trust `ws.onclose`. When the coord process dies and is relaunched,
a TLS-terminating front door keeps the worker-side TCP ESTABLISHED, so `ws.onerror`/`ws.onclose` NEVER fire and `ws.send`
(including in-band JWT refresh) black-holes forever; the restarted coord's in-memory `connectWorkers` registry
has no WS for the fingerprint → the hub socket lookup returns null →
the coordinator's spawn handler throws failed_precondition on every spawn while heartbeats (a
separate unary transport) keep the row looking alive.

**Right** — **a stale-link watchdog on the worker side** (`crates/roost-worker/src/runtime/link_loop.rs`,
policy in `crates/roost-worker/src/backoff.rs`): coord pings on a fixed keepalive interval; every
downstream frame stamps `lastDownstreamAtMs`; a per-dial interval (`STALE_CHECK_INTERVAL_MS` 15s) force-closes
and re-dials after `STALE_LINK_TIMEOUT_MS` 90s (3 missed pings) of downstream silence → hello→snapshot replay
heals the rest. Same half-open-behind-a-proxy class as the boot RPC timeout.

**Guard** — `crates/roost-worker/tests/worker_reconnect_ladder.rs` —
`silence_past_the_timeout_is_stale_and_a_frame_resets_it`; `crates/roost-worker/tests/backoff_policy.rs` —
`a_silent_link_is_stale_rather_than_idle`, `the_stale_window_is_a_whole_number_of_keepalive_intervals`.

### Cold start loses the event published between snapshot and socket

**Symptom** — "brand-new browser: spawning a terminal does nothing — no pane, no sidebar row, store `sessions` stays empty until a reload / 'works on the second load'"

**Wrong** — dial the Sync socket after the bootstrap lists again (throws away the cold-start win for every warm
boot to fix only the first-ever boot), or paper over it with a post-bootstrap `sessionsList` refetch.

**Right** — **the snapshot must be ordered AFTER the socket is subscribed.**
Coordinator Sync runs no backfill from zero, so an event published between
`sessionsList` resolving and the socket subscribing is lost outright — there is
nothing to replay it from. The client core's hydration (`crates/roost-client-core/src/sync/hydration.rs`)
awaits the subscribed barrier (the subscribed wait,
which resolves only once the subscription establishes socket/domain
generations, never at `WebSocket.onopen`) and takes its snapshot against that
socket's id, so the window is CLOSED, not merely shrunk. Browser authorization
finishes separately through one-shot grant redemption or pairing before this
pipeline proceeds; an unknown key remains in onboarding and is not silently
enrolled or retried as a transport failure.

**Guard** — `crates/roost-client-core/src/sync/hydration.rs` — `the_probe_runs_once_per_dial_after_the_subscribed_wait`.

---

## Coordinator RPC, audit and data integrity

### A fresh browser's key is not authorized yet

**Symptom** — "browser 401 on workers.list after fresh context"

**Wrong** — infer authority from loopback, a tailnet source address, or a
retired implicit-enrollment route.

**Right** — a fresh browser must redeem a scoped one-shot browser grant
(`#pair=<bearer>` or pasted token), or post a pairing request that an
already-authorized browser or direct on-host operator explicitly approves.
Network position supplies reachability only. Quickstart preserves one-command initial
use by opening a host-minted fragment grant after tenant initialization.

**Guard** — none — the v2 guards `apps/coord/tests/device-revocation.test.ts` (a tailnet address
without a grant remains unauthorized) and `apps/coord/tests/pair-bus-publish.test.ts`
(a tailnet caller cannot self-approve) left with the TypeScript tree.

### audit_log caller_fp is NULL for every authed RPC

**Symptom** — "audit_log shows caller_fp=NULL for every authed Connect RPC"

**Wrong** — writeAuditLog from the outer fetch wrapper in `coord-factory.ts` — the auth interceptor sets
caller_fp on per-RPC contextValues which the outer wrapper can't see; bridging via AsyncLocalStorage works but
the indirection rots on the next async-layer addition.

**Right** — **write the audit row INSIDE the authenticating layer** (`crates/roost-coord/src/middleware/audit_layer.rs`).
The layer has the caller (just verified), the path
(`/${service}/${method}`), the trace id (header) and the status (200 on success; the mapped HTTP status on a
ConnectError throw). `coord-factory.ts` only audits non-Connect paths (db-export, SPA, 404) where a null caller
is structurally correct.

**Guard** — none — the v2 guard `scripts/lint-roost.ts` (audit-inside-the-interceptor rule) left with the
TypeScript tree.

### A mutation commits without publishing its bus delta

**Symptom** — "task state changes invisible to other browsers — Browser A claims/done, Browser B's QueueView keeps showing prior state until refresh"

**Wrong** — enqueue publishes `created`; the next-pending / set-state / cancel handlers do their DB UPDATE but
never publish, and sync-stream backfill by event id doesn't recover it because in-memory bus deltas aren't in
the events table.

**Right** — **publish the state at every UPDATE-returning point.** Every mutation handler whose domain has a bus
MUST follow its committed update with the matching publish. Bus message shapes live in
`crates/roost-coord/src/events/bus_messages.rs`.

**Guard** — `crates/roost-coord/tests/sync_feed_bus_coverage.rs` — `every_bus_in_the_coordinator_has_a_producer`.

### Rate-limit buckets matched by path prefix

**Symptom** — "rate-limit prefix matches read-only list calls — bootstrap traffic + tab focus refresh burn the same bucket as mutations, 429-cascade on legitimate writes"

**Wrong** — path-prefix match (one prefix catches both List and the mutations) plus an
`if (req.method === 'GET') return null` bypass — but Connect-ES emits every unary RPC as POST, so the bypass
never triggers.

**Right** — **an explicit set enumerating mutation paths only** in
`crates/roost-coord/src/middleware/rate_limit.rs`: auth (MintBootstrap/RedeemWorker/RedeemBrowser),
workspace create/update/delete/set-sessions, task enqueue/set-state/cancel, MCP mutations, and worker
rename/delete/deploy-start. `*List`, identity and health probes are NOT in the set.

**Guard** — `crates/roost-coord/tests/middleware_rate_limit.rs` — `an_unlimited_method_never_opens_a_bucket`,
`a_client_spends_one_budget_across_its_connections`.

### A coordinator-global setting stored in a dashboard scope bricks self-hosted boot

**Symptom** — "fatal: self-hosted tenant invariant violation: app_settings contains invalid dashboard
scope" — the coordinator exits at startup on a database whose `push.vapid` keypair carries a
`dashboard_id`.

**Wrong** — relax the guard, or hand-delete the offending row on the live database. Also wrong: the
drift that causes it — writing `push.vapid` with a `dashboard_id`, when `crates/roost-coord/src/push/vapid.rs`
reads and writes that keypair only at the explicit NULL scope, so a scoped copy is unreachable by
every code path that exists.

**Right** — the guard is correct (the self-hosted tenancy guard owns it), so repair the data in a
numbered migration that drops the unreachable
scoped copies, and promotes the newest one to NULL scope when no global row exists rather than
discarding the identity that signed the live push subscriptions. A coordinator-global setting belongs
in the NULL scope; every dashboard-scoped key stays scoped.

**Guard** — `crates/roost-coord/tests/push_vapid_scope.rs` — `a_per_dashboard_vapid_row_is_neither_read_nor_written`,
`the_identity_is_written_once_to_the_coordinator_global_row`.

---

## Browser platform reality

### lib.dom types are the spec surface, not the engine's

**Symptom** — "a DOM option silently does nothing in the browser while `tsgo` is green / `<input capture>` opens the file browser instead of the camera / an assignment to a documented DOM property never reaches the attribute"

**Wrong** — trust the type checker: lib.dom declares the property, so the assignment typechecks and reads as
done. Equally wrong once it misbehaves: widen the type, cast to `any`, or relax an unrelated header
(`permissions-policy: camera=()` does NOT gate `<input capture>`) — the checker was never the problem.

**Right** — **for any HTML attribute whose IDL reflection is not universal, set the ATTRIBUTE
(`input.setAttribute("capture", …)`) and assert `getAttribute` in a test.** A
green typecheck is not evidence that a DOM property exists at runtime, and a browser-only no-op has no stack
trace, so unit tests that never touch a real engine stay green. The tripwire must assert the ABSENCE first
(`expect("capture" in makeInput()).toBe(false)`) or a fake DOM that later grows the field silently retires it.
Measured in the live tab: `"capture" in document.createElement("input")` → **false** on Chromium 150, so the
property assignment became an expando and `getAttribute("capture")` stayed `null`.

**Guard** — none — the v2 guard `apps/web/tests/attachmentsPicker.dom.test.ts` left with the TypeScript tree.

### A Dioxus `on<event>` attribute whose rsx name is not the browser event name is dead code

**Symptom** — an affordance wired straight to the DOM ("double-click the rail to reset the
sidebar") that never once fires in a browser, with no warning anywhere. The element is present, the handler is
registered, `pointerdown`/`keydown` on the SAME element work, and the only visible evidence is that the state
the handler would have written never changes.

**Wrong** — look for an event-ORDERING problem, a later gesture overwriting the write, or a stale captured
width. All three were ruled out in the measured case: the whole run carried exactly three
`shell intent set_sidebar_width` events, so nothing overwrote anything and the handler simply never ran.
Equally wrong: reach for the deprecated `ondblclick` alias, which happens to work and leaves the next reader
with an attribute that looks like a typo.

**Right** — **the matcher compares the rsx attribute's own tail against `event.type_()`, so an `on…` name
that is not byte-identical to the browser's event name registers a handler that can never be invoked.**
`dioxus-html-0.7.10/src/events/generated.rs:246` maps `ondoubleclick => dblclick` (and `:243-244` the
deprecated `ondblclick => dblclick`); `dioxus-web-0.7.10/src/dom.rs:91` supplies the browser's `dblclick`;
`dioxus-core-0.7.10/src/runtime.rs:435` (bubbling) and `:494` (non-bubbling) match with
`attr.name.get(2..) == Some(name)`. `"ondoubleclick".get(2..)` is `"doubleclick"`, which is not `"dblclick"` —
so the two spellings differ by exactly the character the matcher is sensitive to, and the deprecated alias
matches only by accident. A handler that cannot be called has no stack trace and no type error, so the tripwire
is the DECISION, not the attribute: decide a multi-press gesture from the pointer stream
(`sidebar_resizer.rs`'s `PressTracker` pairs two primary presses inside a window and returns a pure
`PressOutcome`), which is testable natively and does not depend on a name mapping at all. When an attribute
must stay, verify its tail against the browser name before trusting it.

**Guard** — `crates/roost-web/src/components/layout/sidebar_resizer.rs` —
`the_second_press_resets_to_the_default_and_retires_the_drag` pins the two-press → reset decision and the
retirement of the in-flight drag, which is the behaviour the dead attribute stood for. The smell to grep for is
any `on…` attribute whose rsx spelling is not the browser event name — `grep -oE 'on[a-z]+' … | sort -u`
against the `dioxus-html` table.

### An unbounded await in the device-open path parks forever

**Symptom** — "mobile mic records once then never again / stop leaves the UI animating / phone recording indicator stays lit until reload"

**Wrong** — treat it as a network problem (the socket-open timing says the socket was fine), or as the
silent-mic class — the silence watchdog is armed FROM capture's resolution, so a start that never resolves has
nothing watching it; equally wrong: lengthen the mobile idle window so tap #2 reuses a warm pipeline, which only
hides the cold re-open that re-rolls the WebKit dice.

**Right** — **every await in the device-open path is bounded and a failed open disposes what it built.**
`micTimeouts` (open/resume/module) in the voice capture (`crates/roost-web/src/voice/`) wraps `getUserMedia`,
`AudioContext.resume()` and `audioWorklet.addModule()` — WebKit returns promises that NEVER settle while the OS
audio session is mid-transition, and an unbounded await left the warming slot non-null for the page's lifetime
(every LATER tap awaited the same dead promise) and the starting-captures count above zero forever (so
`releaseMicIfIdle` never released the device). `openPipeline` builds into LOCALS and publishes the singleton in
one step, so a stalled open that settles late cannot clobber the pipeline a later tap already built. Every async
continuation in a recording carries a run token (`crates/roost-web/src/voice/state.rs`, bumped in teardown),
because completing a send resets the end-intent to null and null ALSO means "a recording is live" — that is how
a stopped recording's grant opened a socket onto the shared connection and killed the NEXT recording. Finalizing
has a watchdog and stays tappable.

**Guard** — `crates/roost-web/src/voice/state.rs::RunFence` — a continuation carrying a retired run token is
refused, and one recording spends exactly one settle (`begin_recording`/`current`/`admits`/`claim_settle`),
which is the pair of properties the old run token and the duplicated finalise both broke;
`crates/roost-web/tests/voice_draft_settle.rs` — three consecutive recordings settle to exactly what the first
two committed, the finalize deadline cannot settle twice, and an unmounted composer leaves the draft it
started from.

### An unproven hypothesis is persisted and comes back as ordinary text

**Symptom** — a dictated draft returns carrying the words the recogniser only GUESSED
(`"hello from the mic hello from the mic still recording"`), after a pane switch, a drawer opening, or a
reload — while the live field shows exactly the right thing the whole time.

**Wrong** — fix it in the dictation engine, or in the draft-restore path. The paint is correct and must stay
correct: the operator watches an interim and revises it mid-utterance, which is the entire affordance. Equally
wrong, and tempting because it is one line: stop writing the interim into the draft at all — that removes the
feature. The defect is that PERSISTENCE treated a hypothesis as a decision.

**Right** — **the interim and the operator's own words are two different things, and only the second is worth
storing.** `DictationBinding::show` paints a composed value whose tail is provisional, so the composer's save
effect — which persists on every re-render — captured the guess. `voice/transcript.rs::PaintedDraft::persisted`
is the boundary: it keeps the confirmed HEAD and drops the tail, and a mark that is stale or not aligned to a
character boundary is ignored rather than obeyed, so a malformed interim can never truncate real text.
`composer.rs` calls it from the save effect. This is the same shape as the "a client overlay never blocks
reconciliation" rule: what the operator SEES and what the system BELIEVES are different questions, and a
rendering decision must not be persisted as a decision.

**Guard** — `crates/roost-web/src/voice/transcript.rs` — an ordinary draft is stored byte-identical, only the
unspoken tail is dropped, a stale or misaligned mark is ignored rather than obeyed, and the FIELD still shows
the whole hypothesis; `crates/roost-web/tests/voice_draft_settle.rs` — an unmounted composer leaves the draft
it started from, stored and displayed.

### A diagnostic sink throws into the path it was observing

**Symptom** — "terminal input silently dies once SPA diagnostics are on" /
`TypeError: Do not know how to serialize a BigInt` / `JSON.stringify cannot serialize cyclic structures`,
thrown from a `diag()` call inside the send path.

**Wrong** — `JSON.stringify(kv)` raw in a diagnostic sink, then repairing the one call site that blew up
(`input_seq: String(pending.inputSeq)`). A proto `uint64` is a `bigint`, so the sink threw out of
`diag("bytes.up_send", …)` back into `sendTerminalInput` and killed every keystroke; per-call-site
stringification leaves every future call site armed with the same trap.

**Right** — **observability can never propagate a failure into a product path.** The canonical JSON boundary
(`crates/roost-protocol/src/json.rs`) maps `bigint`-sized integers to their exact decimal string at every
depth — rounding through a float is forbidden, it loses precision past 2^53 — and returns the caller's
fallback instead of throwing, so the event still reaches the operator with the loss flagged. The diag facade
(`crates/roost-observability/src/diag.rs`) wraps record construction AND sink dispatch in one guard per
function that reports with strings only, so a hostile value cannot re-throw on the reporting line. One guard
at the facade covers every sink.

**Guard** — `crates/roost-protocol/src/json.rs` — `a_value_json_cannot_express_falls_back`;
`crates/roost-observability/src/fields.rs` and
`crates/roost-observability/src/diag.rs` — a value `serde_json` refuses is
stored as a flagged string and a throwing sink is reported through the facade
rather than raised.

### env(safe-area-inset-*) is 0px on a television, and a portal escapes the shell's padding

**Symptom** — "buttons/keys are cut off at the edge of the TV screen / I can't see the bottom-right control on
the TV"

**Wrong** — rely on `env(safe-area-inset-*)` to keep chrome off a bezel-cropped edge. TV browsers report all
four as `0px`, so the padding that protects an iPhone notch protects nothing here. Equally wrong: add the
overscan gutter only to `.workbench-shell` — a `position: fixed` surface portaled to `<body>`
(`TerminalNavPad`, `crates/roost-web/src/components/terminal/terminal_nav_pad.rs`) is not inside that box and keeps its
own viewport-relative offsets.

**Right** — **explicit overscan tokens, applied to the shell AND to every portaled fixed surface.**
`--tv-overscan-inline` / `--tv-overscan-block` are declared in `crates/roost-web/assets/styles/theme-vars.css` (~2.5% of a
1080p frame) and applied under `[data-tv="true"]` in `crates/roost-web/assets/styles/tv.css`. When raising a fixed
surface's `bottom`, raise any `max-height` that subtracts a literal mirroring that offset — `.term-nav`
subtracts a `220px` twin of its own `bottom`, so a raised offset without a matching subtraction lets the sheet
run off the TOP of the frame. Subtract the block overscan twice: once for the raised bottom, once to keep the
surface's own top edge clear.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### A `Closure` handed to the DOM as a raw function reference is freed while it can still be called

**Symptom** — "Error: closure invoked recursively or after being dropped" in the browser console, once per
event rather than once per failure, from a stack that starts at a DOM delivery rather than at your own code:
`ResizeObserver.u -> wasm-function -> __wbindgen_throw`. The feature it was supposed to track updates exactly
ONCE — at the moment the registration was installed — and never again, with no Rust panic and no test failure
anywhere. In this tree the compact composer dock published its measured height at mount and lost every later
resize, so the shell's reserve froze at the height the dock had when it first appeared. The same string is also
raised for the RE-ENTRANT case (a callback called again while it is already running), so read the stack before
assuming which half you have.

**Wrong** — `Closure::wrap(...)` written inline as an argument:
`ResizeObserver::new(Closure::wrap(Box::new(f) as Box<dyn FnMut()>).as_ref().unchecked_ref())`. The `Closure`
is a temporary: bound to nothing, destroyed at the end of that statement, while the registration it was handed
to outlives the function. `std::mem::forget(observer)` then keeps the observer for the life of the page with
nothing left on the other end of the call. Equally wrong, and the tempting "fix": leak the callback too. That
is memory-safe, and it is how this bug shipped the second time — a callback nobody owns next to an observer
everybody does, growing by one registration per mount, with nothing saying which of the two is meant to end
the relationship.

**Right** — **a `Closure` handed to the DOM as a raw function reference needs a NAMED owner that outlives the
registration and unregisters before it dies.** Store the observer and its callback together in the thing that
owns the registration — `ComposerSlot { observer: RefCell<Option<(ResizeObserver, Closure<dyn FnMut()>)>> }`,
the shape `pane_mount/browser.rs` already uses for `browser.resize_observer` — and `disconnect()` before the
pair is released, both when a registration is replaced and in `Drop` for the owner, since fields drop after
`Drop::drop` returns. Releasing a `ResizeObserver` handle on its own does NOT stop it: the observed node can
still deliver into a freed callback. This is not the "lib.dom types are the spec surface" entry's class of bug
and has nothing to do with the DOM's view of ownership; it is ordinary Rust lifetime, which wasm-bindgen can
only express by making the callee outlive the caller. The "a defaulted injectable host function loses its
receiver" entry in this section is a receiver that arrived too late; this is a callee that leaves too early.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed. A freed wasm-bindgen thunk is a
JavaScript exception inside the browser; `cargo test` compiles the same ownership chain on a target with no DOM
and cannot observe it, so the only evidence is the live run. The
ordering discipline itself is reviewable rather than testable: any `Closure::wrap` whose result is not bound to
a named owner is the smell to grep for.

### A Dioxus `style` string keeps every property the previous render set

**Symptom** — "revealed terminal ignores the wheel / clicks / selection", "pane stays rounded or clipped after
the spotlight closes", "a slot or bar stays translated off-screen after a swipe"; in a trace DOM snapshot the
element's inline style carries a property its current state never sets (a revealed deck slot read
`visibility: inherit; z-index: 2; pointer-events: none;`).

**Wrong** — computing a `style` string per state and letting each branch declare only what it needs, the way a
Solid style OBJECT may: Solid removes keys the new object lacks, but Dioxus 0.7 does not. Its setter
(`dioxus-interpreter-js-0.7.10` `src/ts/set_attribute.ts:67-84`) snapshots the old inline properties, writes
the new string, then puts back every old property the new string did not set. A parked slot's
`pointer-events: none` therefore survived the reveal, the wheel fell through the visible pane, and the reader
could never scroll into history. An EMPTY string does not reset anything either: it restores them all.

**Right** — every branch of a style function declares every property any sibling branch declares (reset values
like `pointer-events: auto`, `overflow: visible`, `border-radius: 0`), built through one struct with a field per
property so a branch cannot omit one (`crates/roost-web/src/components/deck/terminal_deck_geometry.rs`
`SlotStyle`); or the attribute is REMOVED (`style: None`) when the state has no style at all
(`crates/roost-web/src/components/deck/pane_tab.rs`). Inline style written imperatively onto a Dioxus-styled
node survives re-renders for the same reason, and two sites rely on that (`--term-chat-pane-rest`,
`--cell-cols`).

**Guard** — `crates/roost-web/tests/deck_geometry.rs`
`every_slot_placement_declares_the_same_properties_so_no_state_outlives_itself` (the parked, plain and spotlit
placements declare one property set).

### A multi-line draft leaves the desktop pane: the composer is clipped and its first lines are hidden

**Symptom** — "the chat box goes out of the frame with multiple rows / it gets deformed / you can't scroll
inside it"; on a desktop pane the pill's bottom edge and controls sit below the pane's bottom edge, the field's
first lines are cut off at the dock's top edge, and the dock (`.term-chat__dock[data-placement="pane"]`) computes
`overflow: auto` while carrying `data-size-constrained="false"`.

**Wrong** — writing the constrained flag as `"true"`/`"false"`. The stylesheet matches
`[data-size-constrained]` by PRESENCE (v2 used `toggleAttribute`), so every dock was in the scrolling mode: the
box laid out downward from the dock's one-row resting height, clipped by it and by the pane. Equally wrong:
raising the field's cap or the dock's height to make the draft fit — that takes rows from the grid, and a PTY
height change duplicates rows into history (the "Transient chrome resizes the PTY" entry).

**Right** — the attribute exists only while the dock is constrained (`PaneMeasurement::constrained_attribute`,
removed otherwise), so the box overflows UPWARD over the display, and `CellTerminal` translates the display by
the measured growth in literal pixels — a custom property set on the dock never reached the display, its
sibling. The field's growth is capped in CSS at the smaller of `--term-chat-field-max-lines` lines and
`--term-chat-field-room-share` of the room it floats over (`--term-chat-pane-height`, published by the pane
geometry, or the viewport above the soft keyboard), and scrolls inside itself past that.

**Guard** — `crates/roost-web/src/components/terminal_chrome/pane_geometry.rs`
`an_unconstrained_dock_carries_no_constrained_attribute_at_all` and `a_constrained_dock_carries_the_attribute`.

### The wasm bundle is served uncompressed

**Symptom** — a cold load that spends seconds on one request: `content-length: 59…` (≈5.9 MB) and no
`content-encoding` on `assets/roost-web_bg-*.wasm`, while the `.js` beside it is gzipped.

**Wrong** — the compressible-extension set ported from v2, whose bundle was JavaScript only, so `wasm` was never
in it; and a front door that looked only for `.gz` siblings while dx writes `.br` ones. Equally wrong: run a
`.gz` sibling through the runtime compressor, which answers a body that decodes into the sibling.

**Right** — **`wasm` is compressible and a precompressed sibling is served first, `.br` before `.gz`, read
verbatim** (`crates/roost-host/src/spa_path.rs` `asset`, consumed by `crates/roost-coord/src/http/spa.rs` and
`crates/roost-worker/src/door/spa.rs`). Runtime gzip applies only to an identity file; nothing compresses
brotli at runtime.

**Guard** — `crates/roost-coord/tests/middleware_spa.rs`
`the_wasm_goes_out_as_its_brotli_sibling_then_its_gzip_sibling_verbatim` and
`a_wasm_without_siblings_is_gzipped_on_the_wire`; `crates/roost-worker/tests/local_door_spa.rs`
`the_wasm_goes_out_as_its_precompressed_sibling_verbatim`.

---

## Product boundaries and process

### Roost never owns the agent session

**Symptom** — "omp launcher stops opening in a normal terminal"

**Wrong** — spawn the agent CLI as a headless child, vendor its runtime, or import an agent browser UI.

**Right** — the agent CLI runs as an ordinary command in a normal shell PTY; Roost transports that terminal and
never spawns, supervises, or owns the agent session.

**Guard** — `none`.

### A removed migration's history row reads as corruption

**Symptom** — "Applied migration history is not an exact prefix of embedded migrations: found
`<name>` at position N" — the coordinator refuses to boot against a database that was working
minutes earlier, right after the checkout it runs from moved forward.

**Wrong** — treat every `_migrations` row that is absent from the embedded set as a corrupt
history, or delete the row to make the check pass. Also wrong: deleting a shipped migration file
and reusing its slot number, which is what puts a database in this state.

**Right** — a removed migration's row is TRUE: that database did apply it. List the name in
the declared retirement list (`crates/roost-coord/src/db.rs`) and compare the surviving rows only, never by
position in the raw history — a reused slot number means the retired name can sort before the
migration that replaced it. An unknown name must still fail closed.

**Guard** — `crates/roost-coord/tests/migration_history.rs` —
`a_declared_retirement_is_not_corruption_and_the_rest_of_the_chain_still_applies`, with
`an_undeclared_history_row_is_still_refused` as the fail-closed control.

### Redesigns discard previous fixes

**Symptom** — "sidebar redesign loses every previous fix"

**Wrong** — "phase-N: complete sidebar rewrite".

**Right** — additive commits behind a flag; the smoke flow must still pass after each.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

### An anonymous 401 from the internet writes a row nothing ages out

**Symptom** — `audit_log` grows without bound on a coordinator behind a front door, filled with
`status=401` rows whose `caller_fp` is NULL; the sweep runs and deletes none of them.

**Wrong** — delete the anonymous-401 skip in the audit policy (`crates/roost-coord/src/middleware/audit_policy.rs`) because its
body reduces to a constant once the listener it named is gone, or answer the growth by widening
`AUDIT_SWEEP_METHODS`. Both read as simplification and both re-open the hole: the retention sweep
is an explicit allowlist (`SessionsInput` only) that must never
age out auth rows, so an unauthenticated scanner's row is permanent.

**Right** — keep the predicate and skip exactly the anonymous 401 that arrived through a trusted
proxy. It carries no identity — `audit_log` has no address column — so it is unbounded volume with
no forensic value, while a 401 that names a device, any other status, and every request on a
`direct` listener still persist. Telemetry counters and cooldown-coalesced signals cover the
anomaly the rows would have shown.

**Guard** — `crates/roost-coord/tests/middleware_audit.rs` — `an_anonymous_refusal_is_kept_except_on_the_front_door`.

---

### An incident bundle reports a layer "unavailable" that actually sent its evidence

**Symptom** — the incident replay (`replay-terminal-incident <bundle>`) prints `section browser:
unavailable` (or `section coordinator: unavailable`) with an `omitted worker.remote:<layer>.<field>`
line, even though `terminal.capture_started` recorded that layer as armed. Attribution then reads
`first_divergent_layer none` because three of the four layers are absent.

**Wrong** — treat the omission as a size or timing problem and widen a budget, or relax the
write-side validator so the section stops being rejected. Both bury the real fault: the layer's
evidence ARRIVED and was thrown away because its shape was wrong. `remoteSectionOmission` names a
validator field PATH, and that path is the diagnosis — `browser.captured_at_ms` means something
lacking `captured_at_ms` was placed where a section belongs.

**Right** — an evidence payload is an envelope PLUS the layer's section NESTED under a member named
for that layer (`{schema, layer, capture_id, recording_id, session_id, trigger?, <layer>: {…}}`).
Never flatten a section onto its envelope and never forward the envelope as the section: a flattened
payload passes an envelope check and then fails as a section, so a whole layer disappears with
nothing but one omission line to show for it. `checkTerminalCaptureEnvelope` proves the nested
member exists and returns it, and every producer builds its payload from
`TerminalCaptureBrowserPayload` / `TerminalCaptureCoordinatorPayload` so a flat literal does not
compile. The same rule is why `historyRangesFromBrowserEvidence` reads through
`envelope.browser`: against the envelope it silently found no rows and fell back to the worker's
own tail.

**Guard** — `crates/roost-protocol/tests/terminal_capture_envelope.rs` pins both halves:
`a_flattened_payload_is_refused_at_the_envelope_naming_the_layer` and
`an_envelope_placed_where_a_section_belongs_fails_the_bundle_gate`. Producer-side,
`crates/roost-worker/tests/capture_evidence.rs` — `both_nested_remote_payloads_land_as_their_own_bundle_sections`,
`a_flattened_payload_with_no_nested_layer_member_is_refused_outright`.

### One viewer re-attaching costs every coordinator viewer a second baseline

**Symptom** — a paused or revealed pane resumes and the browser counts two complete cell
baselines where it asked for one; reconnect bytes double for every viewer of that session, not
just the one that re-attached.

**Wrong** — treating the worker's `TerminalViewScreenPort.seedSocket` as per-socket for a
coordinator-relayed socket. The worker has exactly ONE `"coord"` cell sink shared by every remote
viewer, so `requestFull` there is a stream-wide re-baseline; the coordinator's screen replica then
seeds the re-attaching socket as well, and the viewer sees both.

**Right** — a relayed socket is seeded from the coordinator's own replica, so
`seedSocket` returns false without requesting anything; only a LOCAL socket, which owns a
dedicated `local:<socketId>` sink, forces a full. The coordinator asks for a source full only when
its replica genuinely cannot serve the socket AND it was already on that stream — a brand-new
stream arrives with its own baseline from `applyTerminalStreamState`, so requesting one there
re-creates the double.

**Guard** — `crates/roost-coord/tests/terminal_view_owner_screen.rs` —
`a_lease_heartbeat_on_an_attached_view_pushes_no_second_baseline`,
`a_socket_attaching_to_a_held_stream_is_seeded_from_the_replica`,
`a_new_stream_id_asks_the_owner_for_no_source_full`.

---

### A worker-owned session never repairs its replica, and the pane stays blank

**Symptom** — with the worker owning terminal views, a dormant pane returning or a dropped
upstream delta never obtains a source full; the coordinator's replica stays invalid and the viewer
waits forever.

**Wrong** — assuming the repair path still runs. `TerminalScreenHub`'s snapshot request lands in
`TerminalViewStreamController.requestFull`, which holds no session for a worker-owned one and
returns early. Owner mode bypasses that controller by design, so the repair silently disappears.

**Right** — route a session the controller never minimized to the owning worker through the owner
relay, which sends `DTerminalSnapshotRequest`. Whenever a coordinator-owned path is bypassed for
owner mode, audit what ELSE that path was the only caller of; membership was the intended
bypass, repair was collateral.

**Guard** — `crates/roost-coord/tests/terminal_view_owner_screen.rs` —
`a_lost_baseline_is_repaired_by_the_owning_worker_once`.

---

### A predicted character flashes the wrong glyph while typing fast

**Symptom** — typing quickly into a terminal pane paints a wrong character at a cell for a
fraction of a second before the authoritative frame replaces it; the prediction snaps back, and the
glyph shown is the one an EARLIER keystroke is about to occupy that column with.

**Wrong** — judging a prediction against any frame with a later sequence number. The frame the
predictor sees is the fully folded canonical viewport, so every row is present and every arriving
frame judges every in-flight prediction: while "abc" is in flight, the echo frame for `a` confirms
`a` (raising `confirmedEpoch`, which makes `b` count as SHOWN inside the same loop) and then
contradicts `b`, hard-resetting the burst. Compounding it, `resetAll` did not re-arm the confidence
gate, so the next keystroke was painted immediately at the stale authoritative `cursorCol` — behind
the un-echoed input. That mis-anchored, immediately-shown guess IS the wrong glyph.

**Right** — a prediction may only be CONTRADICTED by grid state that could already hold its echo:
`Pred.ackedMs` is stamped from the client-side input admission (`noteInputWritten`, fed by
`InputAdmission.result`), an unacked prediction is never contradicted, and a contradiction must
outlive `ECHO_GRACE_MS`. A proving MATCH is judged unconditionally and credits immediately, even
before the ack lands, so the burst unlocks at the first echo frame and the fix costs no latency —
`judgePrediction` (`predictiveEchoGrid.ts`) is the single place those two asymmetric rules live.
Every reset — contradiction, expiry, alt-screen, paste — ends in `becomeTentative()`, so a guess
anchored on a lagging cursor is never painted. Do not respond to a surviving flicker by widening
what counts as a contradiction; raise the grace instead.

**Guard** — `crates/roost-client-core/tests/predictive_echo_ack.rs` —
`an_echo_frame_for_an_earlier_keystroke_never_contradicts_a_later_one`, `a_reset_re_arms_the_confidence_gate`,
`an_echo_that_beats_the_write_ack_still_unlocks_the_burst` and
`a_match_that_reproduces_the_cells_own_text_proves_nothing`.

---

### Sustained fast typing wipes its own predictions once a second

**Symptom** — typing fast in a terminal pane feels laggy in ~1 s cycles, the `Screen catching up`
spinner shows while typing, and `echo.reset` reports `reason: cleared` for a pane nobody touched:
~1 s of instant local echo, then every prediction disappears, then a full round-trip of nothing
painted, repeating for as long as the burst lasts.

**Wrong** — deriving the DOM-reconcile watermark from the predicted caret column. A prediction that
leads the authoritative `frame.cursorCol` made `_markReconciledIfCurrent()` return early for the
whole burst, so `dom_reconciled` froze behind `handler_canonical`, the pane read `catching_up`, and
its `FOREGROUND_DOM_STALL_MS` watchdog (`handleCatchUpStalled`) called `predictor.clear()` — wiping
the overlay, re-arming the tentative gate, and arming a `DOM_RECONCILIATION_PROOF_MS` redial. The
predictions were correct; the watermark was measuring the client overlay, not the DOM.

**Right** — the watermark compares the painted caret against the column the renderer INTENDED to
paint: `this._paintedCursorCol !== (this.predictedCol ?? frame.cursorCol)`, the same expression
`updateCursor()` paints. A client overlay never blocks reconciliation; only real DOM-fidelity
conditions (reader-pending frame, holds, pending render, row/col count, alt-screen, cursor
visibility) do. `ReconcileBlockReason` has no `predicted_cursor` member — a predicted caret is not
a block.

**Guard** — `crates/roost-web-terminal/tests/echo_overlay.rs` —
`a_leading_predicted_caret_does_not_freeze_reconciliation`.

### A terminal proof fails with "strict mode violation: resolved to 2 elements"

**Symptom** — a Playwright terminal spec dies on `getByTestId('terminal-loading-status')` (or any
other per-pane testid) with `strict mode violation: … resolved to 2 elements`, one card reading
`data-phase="loading"` and the other `data-phase="complete"`. It passes alone and fails in a
multi-pane case such as `smoke/terminal/terminal-switch-perf.spec.ts` "the deck mounts a bounded
number of panes".

**Wrong** — treating the second element as a leak and hunting for the card that "failed to
unmount", or suppressing it with `.first()`. The startup card is per session: every `CellTerminal`
mounts its own, and a card that just completed
stays in the DOM for `FINISH_GRACE_MS + FINISH_HOLD_MS` (300 ms, `TerminalStartupOverlay.tsx`) so
the meter can land on 100% instead of vanishing mid-band. Two cards during a hand-off is the
contract, not a defect, and `.first()` silently asserts against whichever pane the DOM happens to
order first.

**Right** — scope the query to the pane under proof: `page.getByTestId('terminal-slot-<sid>')
.getByTestId('terminal-loading-status')`, and inside `page.evaluate` match on the card's own owner
with `[data-testid="terminal-loading-status"][data-session-id="<sid>"]`. A global
`toHaveCount(0)` is only valid in a single-pane spec.

**Guard** — none — its Playwright spec was deleted with the oracle; a Rust test is owed.

---

### A deleted worker's direct terminal still accepts input

**Symptom** — deleting a worker removes it from the machine list, but an already-open loopback or
WebRTC terminal can still accept PTY input until its grant expires when the worker retirement frame
is lost.

**Wrong** — treat the coordinator retirement frame or `deleteStoreRecord("workers", fp)` as the
only authority fence. The browser still owns a grant, an elected route, and possibly an in-flight
grant mint; none of those live in the worker projection.

**Right** — both the confirmed delete response and the later presence delta call
`applyWorkerRemoval()`. It retires the worker's grant state and in-flight mint, closes every
worker-scoped direct candidate and elected route, emits `worker_retired` so peer retries dispose,
then removes the worker record. Sessions and workspaces remain as offline history.

**Guard** — `crates/roost-client-core/tests/direct_carrier_retirement.rs` "retiring a worker asks
the host to close its carriers" and "a retirement leaves another worker's carrier alone";
and `crates/roost-web/src/platform/carriers/route.rs` `a_worker_retirement_takes_every_connection_it_held`.

---

### A restarted worker stays on Sync with peer phase "grant"

**Symptom** — a worker restart or grant-expiry recovery closes the old direct peer, paints through
Sync, then never elects a replacement; diagnostics remain at `peer_phase="grant"` with
`failure_detail="terminal peer grant changed"`.

**Wrong** — let an old retry timer remain armed after a fresh grant arrives, or drop the current
grant merely because a probe proves the old connection epoch is stale. The timer blocks
`maybeStart()`, and an old-connection callback can erase the replacement grant.

**Right** — a successful grant publication cancels the preinstalled retry timer before starting
the peer, and worker-epoch handling drops a grant only when that grant still names the stale
connection's epoch. Negotiation rejection likewise invalidates only the exact grant that attempted
the failed request.

**Guard** — `crates/roost-client-core/tests/terminal_peer_admission.rs` —
`retries_a_transient_initial_grant_failure_at_the_bounded_retry_deadline`.

### Coordinator-started worker deploys exit 7 from a detached release worktree

**Symptom** — coordinator logs `catchup_failed … "deploy exit 7"` (and `roost doctor` shows `deploy.failed`);
a machine stays "Update available" forever; the Settings → Machines "Update" button fails before touching the
host.

**Wrong** — prove a coordinator-started deploy with `resolvePublishedGitShaOrDie`. It needs HEAD on a branch
AND at the current upstream tip, but a `roost push`-installed coordinator runs from a `git worktree add
--detach` release directory, so `git symbolic-ref HEAD` fails ("source HEAD has no publishable branch") — and
any later `git push` without a `roost push` would move the tip out from under it anyway.

**Right** — `startDeploy` always passes `--coordinator-release --expected-sha=<its own SHA>`, and
`resolveCoordinatorReleaseGitShaOrDie` proves the source checkout is the installed service's
`WorkingDirectory`, that the service's `ROOST_GIT_SHA` is the expected build, and that the clean HEAD matches
it. `roost push` already proved that SHA published before installing it.

**Guard** — `crates/roost-cli/tests/deploy_coordinator_release.rs` —
`a_detached_coordinator_release_at_its_installed_sha_is_admitted`, with
`a_checkout_that_is_not_the_installed_release_is_refused`, `an_installed_build_that_is_not_the_required_one_is_refused`
and `a_dirty_release_tree_is_refused` as the refusals.

### The coordinator refuses to boot on a definition quickstart just wrote

**Symptom** — "coordinator will not start / `config.cf_access_team_domain must be one lowercase label` /
`config.cf_access_aud must be 64 lowercase hex characters` / `ROOST_TERMINAL_PEER_ENABLED must be exactly 0
or 1` / `ROOST_TRUST_PROXY` read as disabled though the operator set it"

**Wrong** — a producer and a consumer, each correct in isolation, disagreeing about what a MISSING value
looks like. `env.get(key)` returns `Some("")` for a variable that is present and empty; the loader passed
that straight into a shape check, and the check refused it. Meanwhile the producer wrote booleans with
Rust's `Display` (`ROOST_TRUST_PROXY=true`) where the consumer reads `== "1"`, and wrote both Cloudflare
Access keys empty when it meant "unset". So `quickstart` produced a definition no `roost coord` would
accept — and on a machine with no Cloudflare Access in front of it, which is every self-hosted install,
that pair is ALWAYS empty.

**Right** — the consumer treats set-but-blank as absence, because a hand-edited unit, an
`EnvironmentFile=` line and `export FOO=` all produce one: `declared_or_absent` in
`crates/roost-host/src/coord_config_loader.rs`, which is what `origin_list`, `is_enabled` and
`normalize_https_origin` already did in the same file. The producer omits a key it has no value for, and
writes booleans as `1`/`0` through `flag()`. These are two different questions — "what do I write?" and
"what can I read?" — and neither layer consults the other, so this is defence in depth across a producer
and a consumer, not duplication. Both halves are required: consumer tolerance alone leaves a misleading
definition, and producer correctness alone leaves one typo away from a coordinator that will not start.

**Guard** — `crates/roost-host/tests/coord_config_blank_settings.rs`:
`a_blank_cloudflare_access_declaration_is_absence_rather_than_a_refusal`, plus the two that fail if the
filter is ever written as "skip validation" instead of "skip empties" (a real pair is kept whole; a
present-but-malformed pair is still refused). The CLI half is pinned by
`a_dry_run_of_a_rerun_keeps_the_installed_front_door` in `crates/roost-cli/tests/quickstart_dry_run.rs`.

**Why this is an entry and not a commit body** — the boolean bug was found and fixed, the test was
rerun, and the rerun's NEXT assertion failed. Stopping at the assertion you were given finds the surface.
The cause was in a layer neither the failing test nor the fix lived in.

---

## Process rule

When a user-reported symptom matches an entry above, fix at THAT layer first. If the entry describes a
different fix pattern than the one the immediate code tempts you toward, the entry wins — it was written
after the tempting fix already failed. Add a new entry only after a NEW root cause is confirmed AND a
regression test (or a `cargo xtask lint` check) exists for it; an entry without a guard is a promise
the repo cannot keep.

### A guard that greps for a name accepts the producer as the consumer

**Symptom** — a reachability test that searches `src/` for a file mentioning `<field>`
(or `<Type>`) without *declaring* it passes green while nothing consumes the value. The
first version of this guard was six of six green, on a capability with no execution
path: `crates/roost-coord/src/events/append_transaction.rs:78` **builds**
`snapshot_reap_ids` without declaring it, so the producer satisfied a test written for
a consumer.

**Wrong** — grep for the name. A name has a producer and a consumer and grep cannot
tell them apart, so the test's subject is the *string* rather than the **direction of
the data**. It reports success while looking like it tests the right thing, which is
the worst of the available combinations: a green suite resting on a false claim.
Merely tightening the pattern does not fix the category.

**Right** — grep for something only the CONSUMER can do. Here: does anything set
`defer_snapshot_reap: true`? Only a caller constructing `AppendOptions` to defer can
write one; the declaration (`events/append.rs:274`), the `Debug` field (`:284`) and the
read inside `build_result` (`:367`) are the only other mentions of the name and none of
them can produce a `true`. That question is unambiguous for a structural reason, not a
stricter-pattern reason.

**THE RULE, because this is the second time tonight and the same author wrote both:**
a test whose subject is a **value** asks whether the value is right; a test whose
subject is a **path** asks whether anything walks it. Those are different questions and
only the second survives a green run. `green` and `correct` are different properties and
the exit code only shows you one of them.

**Guard** — **cited by COMMIT, not by file**, because the file is about to move.
`c02cb9dc` introduced the assertion inside `crates/roost-coord/tests/event_publication.rs`;
it is being moved to a binary of its own, `event_reachability.rs`, because **the
guard's subject is reachability and that file's subject is the deferred reap** — they
shared a file only because the second conjunct was written next to the first. **A
commit is a fixed point and a file is not**, which is why this entry names the commit
first. The assertion's own body at `c02cb9dc` is the authority; wherever it lives
now, that commit holds it.

For reference, as of `c02cb9dc` it lived at:
`a_deferred_reap_waits_for_the_callers_readiness_barrier`, **at `c02cb9dc` on
`v3-coord`. It is not on `v3` until the 2C-GATE merge.**

**Read that twice, because it is this entry's own trap.** A test of the *same name*
already exists on `v3` at `event_publication.rs:288`, and its body is the value-only
version — the camouflage this entry is about. **So following this Guard on `v3` today
lands on a green test that does not check the path: the name matches and the body does
not.** That is why the commit is cited rather than the file. A file and a test name are
not a citation; a commit is.

**STATUS: CLOSED at `8880699f`, and closed by the fix rather than by a softening.**
At `c02cb9dc` the test was red by design, asking only whether anything sets
`defer_snapshot_reap: true`. It is now **green on both conjuncts** — the flag is
set *and* a production reader of the returned ids exists — measured
`cargo nextest run -p roost-coord event_publication::` **6 passed / 0 failed**,
against **5 passed / 1 failed** on the same binary before the fix. The assertion
was not edited; the two value tests beside it still assert only that the ids
come back correctly, and passing them is still not evidence about this one.

**The reader is at `frame_dispatch.rs:354`, and WHERE it is is the whole entry.**
`self.drain_reaps(&self.handle.worker_fp, &result.snapshot_reap_ids)` is a
**dotted read in a file outside the four named producers**, so it satisfies the
second conjunct by **direction**. Had that conjunct stayed a bare grep — the
guard this entry is about — the green would have been **this same defect
arriving by a different route**: a producer satisfying a consumer's test. The
name joins the list; it does not replace the dot.

**Do not re-open this by adding a second path to the grep.** A second reader is
the fix; a second *mention* is the defect. If a future change moves the read,
update the direction claim here or delete the entry — do not leave a green that
means a mention.

### A smoke spec dies at enrollment with "goto: Download is starting"

**Symptom** — a Playwright terminal spec fails in `smoke/terminal/fixtures.ts:144` with
`Error: goto: Download is starting`, on the **worker-served** page (the trace line is a spec's
`stack.localUiUrl(...)` enrollment, not `stack.baseUrl`). Any spec that opens the local door
reds this way; `terminal-local-fast-path` and `terminal-peer :61` were the two observed. The
coordinator's own page in the same test enrolls normally.

**Wrong** — read it as the host. It reproduced identically with the Rust and the TypeScript
coordinator, on every port, with a freshly built web bundle present — and that symmetry is
the tell. A real host fault does not survive a stack swap. Two leads filed it as "environment,
outside this track" and moved on, which is how a one-line harness defect nearly became a
permanent skip on two specs.

**Right** — Chromium is reporting the truth: the door answered `404` with
`content-type: application/octet-stream`, so a document navigation is a download. The worker reads
its SPA from `ROOST_WEB_DIST_PATH`, and a from-source build has no embedded assets to fall back on.
`smoke/terminal/stack-runtime.ts` set the key for the coordinator child and
`stack-worker-runtime.ts` set it for nobody, so the door had neither a disk build nor embedded
assets. **Both launchers now resolve it identically** from `ROOST_SMOKE_WEB_DIST` (default
`.smoke-pin/web`), so the bundle reaches the worker too. Fixed on `v3` at `a84f4a12`.

**Guard** — the specs themselves, and they are the guard because they fail loudly rather than
skipping: `terminal-local-fast-path.spec.ts` (1 failed → 1 passed) and `terminal-peer.spec.ts`
(4/5 → **5/5**) on chromium, same tree. Do not add a second
assertion for this: the enrollment IS the check, and a new one would pass for the wrong reason.
When one of these reds, read the response in the trace (`0-trace.network`, the snapshot for the
navigation) before blaming the machine — the status and `content-type` name the layer that failed.

### A hand-built JSON reply spells a proto field the way Rust does, not the way the contract does

**Symptom** — "the folder picker is empty / browse shows no rows / a directory
lists zero entries while every piece of chrome around it renders correctly".

**Wrong** — reading it as "the listing did not arrive", and then chasing the
coordinator relay, the Sync domain, or the browser's own fetch. Also wrong:
making the READER tolerate both spellings, which converts a loud wire defect
into a silent one on every all-Rust run.

**Right** — a proto field absent from a JSON object is its DEFAULT, so a
misspelled key does not error and does not skip: it produces the zero value. A
`bool` that means "is this a directory" arrives as `false`, every subdirectory
is classified as a file, and a listing that is nothing but folders renders as
an empty directory. The symptom points at the CONSUMER and the defect is in the
PRODUCER's reply. `browser_commands/file_commands.rs` published
`{"name": …, "is_dir": …, "mtime_ms": …}`; the contract spells it `isDir`,
and a coordinator that reads only `isDir` with no fallback saw the flag false for every entry.

The fix is one typed struct whose field carries `#[serde(rename = "isDir")]`,
so the emit and the sort read the same key and cannot drift again — a
hand-written `serde_json::json!` reply has no type to disagree with itself.
Check the OTHER fields of the same reply while you are there: `mtime_ms` and
`resolved_path` are legitimately snake_case, which is why a sibling reply in
the same file was correct and this one was not.

**Why the tolerance must go, not stay** — `crates/roost-coord/src/attachments/files.rs`
read `isDir` and fell back to `is_dir`. That fallback is why an all-Rust run
never saw this defect: the coordinator quietly read the misspelling. Only the
cross-stack run — TypeScript coordinator, Rust worker — surfaced it, and it
surfaced as three specs reporting an empty listing. A field name is not a
compatibility surface.

**Guard** — `crates/roost-worker/tests/list_dir_wire_contract.rs`, which
decodes the reply the way the CONSUMER does rather than reading the worker's own
keys back: a seeded subdirectory must arrive as a directory, a seeded file must
not, and the two snake_case fields the coordinator also reads must be unmoved.

### Only the FIRST agent status for a session ever reaches a hydrating client

**Symptom** — an agent reports `working`, then reports `blocked`, and the browser stays on
`working` forever. The first report is what the tab badge, the sidebar row, the folder row and
the title prefix all read, so everything downstream is self-consistent and simply stale. The
agent's own log shows both reports accepted, and the coordinator answers
`agentStatusGet` with the blocked state — the loss is on the way to the SOCKET.

**Wrong** — look at the client fold, the revision rule, or the occupant identity. All three were
exonerated by measurement: the fold ACCEPTS the second report (proved by decoding the captured
wire bytes with the real `decode_firehose`), and a lower-or-equal revision being refused is
correct. Equally wrong: re-sending on a timer, or making the fold lenient about revisions — the
frame was never offered to it.

**Right** — **a retained feed sample supersedes a buffered one only when it is at least as NEW.**
`retained_supersedes_buffered` (`sync_ws/send_queue.rs`) compared agent-status frames by SESSION
ALONE, on the premise in its own doc that "a live status frame already buffered can never be newer
than the retained one". That premise is false: an agent reports continuously, so a newer status
lands on the live link WHILE the retained seed is still being assembled, and `coalesce_buffered`
then dropped it as already covered. The retained seed held revision 1 and the client received
revision 1 forever. Compare `OwnedFrame::agent_status_revision()` as well as the session, and treat
a frame with no revision as non-coalescing. The same reasoning applies to every
current-value projection: coalescing is only sound when the retained sample is provably at least
as new as what it replaces.

**Guard** — `crates/roost-coord/src/sync_ws/send_queue_tests.rs` —
`a_retained_status_older_than_the_buffered_one_does_not_supersede_it` is the case that was
silently dropped, plus supersedes-at-equal-revision, never-supersedes-another-session, and
never-supersedes-a-non-status-frame.

### Every terminal stays on the Sync route and `route.active` reads `null`

**Symptom** — a real stack paints the terminal and takes trusted input, so nothing looks
broken; `terminalBrowserSnapshot(sessionId).route.active` is `null`, `peer_phase` is `null`,
and `smoke/terminal/terminal-peer.spec.ts` and `terminal-peer-failover.spec.ts` time out in
`waitForDirectRoute` with `direct route unavailable: {"activeKind":null,…}`. The same session
on the TypeScript coordinator and worker elects a carrier and paints it.

**Wrong** — reading it as a worker that will not peer, a coordinator that refuses the grant,
or a discovery probe that found no door. None of those is what a refusal looks like: each of
them leaves a fault, a `fallback_reason`, or an absent door somewhere in the same snapshot, and
this state has all three empty. A `Signalling` machine's own unit tests all pass in this state,
which is the tell: a state machine that is never CONSTRUCTED cannot mint a credential, cannot
learn which worker shares the page's machine, and cannot open a transport, so every symptom
downstream is a session quietly living on Sync.

**Right** — `CarrierLane` (`crates/roost-client-core/src/client/carriers/lane.rs`) is the one
thing that constructs a `Signalling`, and `Store::direct` is where it lives. Four feeds, and
the symptom is any one of them missing:

1. `handle_view_opened` / `handle_view_hidden` / `handle_view_closed` call
   `store.direct.demand(…)`. Without the `active_views` count, `Signalling::start` returns on
   its first gate and never asks.
2. `pump::carriers::request_grant` reports the mint back as `ClientEvent::DirectGrantMinted`
   — built ONCE and handed to both the dial and the election, so the loopback carrier and the
   peer machine cannot disagree about one reply's deadline, scope and epoch. A refusal is
   reported too, or the grant lifecycle stays `Requested`, which is the one phase with no path
   forward.
3. `pump::carrier_dial::open` dispatches `ClientEvent::LocalDoorAnswered` the moment discovery
   settles, INCLUDING the negative answer. `LoopbackProbe::permits_peer` refuses a peer until
   this arrives, so silence behinds a loopback carrier that was available all along — and a
   machine that finished looking and found nothing must not read as one that has not looked.
4. `handle_carrier_authenticated` and `handle_carrier_lost` report loopback presence; the
   machine's whole view of a loopback carrier is "one is staged", and a faulted peer's fallback
   decision reads that flag rather than the route registry.
5. `pump::peer_lane::perform` acts on EVERY `CarrierEffect` the machine emits —
   `OpenTransport`, the bounded filtered `local_offer`, `NegotiateOffer`,
   `ApplyAnswer`, `StageCarrier` and `Core(effect)` handed to the ordinary effect
   executor — and a refused arm is dispatched back as a `ClientEvent`, never
   dropped. The match is EXHAUSTIVE with no catch-all, so a new core arm is a
   compile error rather than a `tracing::warn!` nobody reads. A host half that
   declines the five peer-lifecycle arms produces exactly this symptom and
   nothing else: every `Signalling` unit test passes, the machine reaches
   `Gathering`, and no carrier is ever staged.

**Guard** — `crates/roost-client-core/tests/direct_carrier_lane.rs`, all six: "a view on a
worker asks for a direct grant" (the failing-before is an empty effect vector with the demand
line removed), "a page served by the worker never allocates a peer", "a page elsewhere releases
one peer for the worker", "closing the view stops the asking and the machine is forgotten",
"a retired worker makes its machine ask nothing further", "a refused mint arms the retry the
election is waiting on"; plus
`crates/roost-web/tests/peer_carrier_attempts.rs` for the host half's own fences
(a `Ready` that does not match the grant is refused; a lane fragment round trip;
a frame from a retired attempt reaching nothing while the other worker's carrier
survives; a promotion token that cannot be reused across attempts).


### An unpaired browser's page sits on "Checking access…" and the pairing panel never renders

**Symptom** — a browser with no paired device key shows `Checking access…` forever. The
Playwright error-context snapshot is one line, `- status: Checking access…`, and the
expectation that timed out is `onboarding-pair-start-btn`; the coordinator's own answer to the
browser's probe is `{"code":"unauthenticated","message":"SessionsList requires a credential"}`
with **no** `x-roost-auth-layer` response header. The Sync socket never opens either — the
upgrade is refused at the handshake, so there is no `4001` close code to classify.

**Wrong** — reading it as a web gate bug and going to `roost-web/src/app.rs` (the
`Checking` → `PairSurface` switch is right) or to `pump/boot.rs` (the probe does fire; the
trace shows it going out, with a bearer, and being refused). Also wrong: opening the gate on a
timer, or having the client treat a bare `Unauthenticated` as a device rejection — a client
that guesses cannot tell a rejected device from a trusted proxy asserting the caller, which is
the entire reason the header exists.

**Right** — `classify_auth_failure` (`roost-client-core/src/client/rpc/auth_failure.rs:70`)
requires three things, and the one the coordinator controls is the marker. The marker is
stamped on a refusal a handler builds, and v2 stamped it for every "authentication required"
(`auth-interceptor.ts:256-262`, `protocol/spec/auth-and-pairing.md` "Errors"). The v3 gate
refuses BEFORE a handler runs, and its refusal named the method and nothing else — so the
contract was met at the handler and broken at the gate, which is why every handler-level test
passed. Fix it where the refusal is built: `rpc/auth_gate.rs::credential_refusal` stamps
`x-roost-auth-layer: device` whenever `AuthRequirement::admits_browser_key`, which is the
credential that WOULD have worked. That predicate is deliberately not
`permission_denied_for`'s: there the principal is known and the marker names the layer that
refused; here nothing resolved, and `DeviceOrOwnWorkerRecovery` (which is what `SessionsList`
records) still has a browser key among the credentials that satisfy it, while a `Worker`
requirement gets no marker at all.

The same answer is what a REVOKED key gets, so a browser that was paired and then revoked
reaches the pairing panel on the same path instead of spinning on a probe it can never pass.

**Guard** — `crates/roost-coord/tests/device_refusal_through_gate.rs`, driven through the
production router: "an unresolvable credential on a device method is answered with the device
marker" (the failing-before is `left: None, right: Some("device")`, which is byte for byte the
recorded response), "a worker-only requirement carries no device marker", and "a paired
browser's probe is answered and names no auth layer".
 
---

### A page a worker served aims every coordinator RPC at the worker

**Symptom** — a page opened on a worker's own loopback origin
(`http://127.0.0.1:<port>`, the SPA the worker serves for a browser on the PTY's machine)
never sees a worker, a session or a workspace: `window.__smoke.state().workers` stays empty and a
spec waiting on it times out in its FIXTURE, before any of its own assertions. The same specs
pass on the coordinator's origin, which is what makes it read as a carrier fault rather than a
routing fault.

**Wrong** — building the Connect client from `window.location.origin` unconditionally
(`pump/boot.rs::coordinator_origin`). A page a worker served has the WORKER as its own origin, and
the worker's door answers the SPA, `/api/local-bootstrap` and the two socket upgrades and refuses
everything else — so every RPC is aimed at a server that never had a coordinator on it, and the
failure is silent because the door's own 404 is an ordinary HTTP answer. v2 does not have this
shape: `connect.ts:69-93` resolves `coordBase()` through `readLocalBootstrap()` first. The Rust
core even carries the rule — `client::local::coordinator_base` prefers a worker-served bootstrap
outright and its doc names this exact hazard — but nothing called it, so the whole override was
dead code with tests passing.

**Right** — prime the serving origin's answer BEFORE the application graph loads, then resolve the
base synchronously from it (`platform/door_probe::prime_serving_bootstrap` +
`coordinator_base_url_for_page`, called from `main.rs` ahead of `dioxus::launch`). The probe is
bounded by `DOOR_PROBE_TIMEOUT_MS` and fail-closed: the coordinator's 404 on
`/api/local-bootstrap` is the ordinary "a coordinator served me" answer, and the page then keeps
its own origin.

**Guard** — the decision rule is pinned natively by
`crates/roost-client-core/tests/local_discovery.rs`; there is no native test of the host wiring,
because the wiring is `window.location` and `localStorage`.

### A named `tracing` target that does not start with the crate name is silently dropped

**Symptom** — a defect on the page side cannot be localised because the browser console carries
no carrier, door, sync or auth line at all. The code is instrumented, the statements are there,
and a Playwright trace contains zero console events, so a stop has to be guessed at from the
snapshot instead of read from the log.

**Wrong** — an `EnvFilter` directive of `roost_web=info,warn` (`install_tracing`). `EnvFilter`
matches a target by PREFIX, and this crate names an explicit target on every event — `carriers`,
`door`, `auth`, `terminal`, 34 of them — none of which starts with `roost_web`. Every one of them
therefore fell through to the bare `warn` default and every `info!` was discarded. The directive
reads as "info for our crate" and is not.

**Right** — decide by BUILD, not by target spelling. A `--features smoke` build is the diagnostic
build — the same feature that installs the `__smoke` backdoor, and one a release build does not
enable — so it gets `info`; a release build keeps `warn`, where the only reader is an operator
reading a console. `RUST_LOG` overrides both.

**Guard** — no behavioural test: a filter is a diagnostic surface, and the observable is "the
trace has console events". `install_tracing`'s filter expression is the thing to re-read after
adding a target, and the check that catches it is running one spec and counting console events in
its `trace.zip`.

---

### A direct carrier can never become the elected route, because staging waits for the frame it earns

**Symptom** — a carrier authenticates, the browser logs `loopback carrier admitted` and
`direct carrier registered`, and then nothing ever happens: no inbound frame, no baseline, and
`snapshot.route.active` reads `"sync"` until the spec's 60-second `waitForDirectRoute` gives up.
`peerPhase` stays `idle`. The terminal is healthy on Sync the whole time, which is why the
symptom reads as "the direct path is slow" rather than "the direct path cannot start".

**Wrong** — staging a `PromotionCandidate` only when the first direct FRAME arrives
(`handle_sync.rs::fold_into_candidate`, the only caller of `RouteRegistry::stage`). That makes the
whole election one cycle with no entry point:

- a worker streams to a socket only once it has been told that socket is watching the session,
  and the only thing that tells it is a `TerminalViewCommand`;
- a `TerminalViewCommand` for a direct link needs a direct token, and the only source of one for
  an unelected session is a staged candidate's token;
- `publish_view` refuses to send on any token `routes.route_matches` does not confirm — correct
  for every other command, and fatal for this one, because it demands the route exist first;
- `promote` refuses until the candidate holds a COMPLETE validated baseline, which needs frames.

So: the frame waits for the command, the command waits for the token, the token waits for the
staging, and the staging waits for the frame. Nothing is ever slow; nothing ever runs.

**Right** — break the cycle where it is closed, on carrier admission rather than on the first
frame. `handle_carrier_authenticated` now calls `stage_viewed_sessions`, which stages a candidate
for every session the carrier's grant admits AND this document is viewing, and publishes that
view onto the candidate's token through `handle_sweep::publish_view_on_candidate`. The worker
streams, the candidate earns a baseline, and `promote` elects it — through its own fence, which
still refuses an incomplete baseline. The grant's scope bounds the command, so a carrier names
only the sessions it holds a credential for.

**Guard** — `crates/roost-client-core/tests/direct_carrier_staging.rs`, driven through
`ClientCore`: `an_admitted_carrier_asks_the_host_for_a_view_id_and_publishes_nothing_yet` (fails
`got []` without the fix — the request is the effect now, not a publish),
`a_carrier_asks_for_ids_only_for_the_sessions_its_grant_admits` (the control: a grant is
scope-bound, so an over-broad staging that named every session is caught too), and
`publishing_a_candidate_does_not_elect_it` (the fence is intact — staging is not electing).

**This is NOT the entry on the unimplemented WebRTC negotiation.** Loopback needs no offer, no
answer and no peer: it dials the worker's own door and authenticates with a grant. This cycle is
entirely below the transport, and it is why an admitted LOOPBACK carrier was already dead before
any negotiation code was reached.

### One view published on two live carriers is refused as owned by another socket

**Symptom** — a direct carrier admits, the browser publishes its view onto it, the worker
receives the command and answers with nothing but a reply: the candidate never gains a baseline,
`snapshot.route.active` stays `"sync"`, and the direct route never wins. In the worker log the
view command is present and the outcome is zero calls and zero changes; on the browser there is
no fault at all, because nothing failed on the way out.

**Wrong** — publishing a view onto a newly staged candidate under the SAME `view_id` it already
holds on Sync. The authority keys a view by `${viewer_key}:${view_id}`, and the viewer key is
`${deviceFingerprint}:${tabId}` — identical on every transport this document owns. So the local
socket's command lands on a key the COORDINATOR socket still holds live, and
`terminal_view::commands::reclaim` refuses with "view is owned by another live socket".
Reclaim is by design for a record whose socket is GONE; it is not a takeover, and the comment on
it says so.

v2 does not have this shape because its candidate never reuses an id: `TerminalPromotionCandidate`
mints a FRESH `crypto.randomUUID()` per view and publishes those PROSPECTIVE ids on the direct
connection, so the canonical's records and the candidate's records are different keys and cannot
contend. `rotateViewTransport` — which releases the old key on the route being left, with an
INACTIVE `TerminalViewCommand` at `cols/rows = 0` and a bumped revision — is a DIFFERENT mechanism
for a later moment, and its own comment names the failure this entry is about: "Two live carriers
sharing one viewer key would otherwise contend for the same worker view record."

**Right** — give the CANDIDATE its own view ids, and leave the canonical's alone. v2 mints a fresh
`crypto.randomUUID()` per view inside `TerminalPromotionCandidate`
(`terminal-stream-promotion-candidate.ts:94`) and publishes THOSE on the direct connection
(`start()`, `:114-123`). The canonical's ids stay live on Sync and are never touched, so there is
no rollback to get wrong: the pane keeps painting on Sync until the candidate commits, and
`applyCanonical` hands the prospective ids to the canonical at the same moment the replica swaps.

**Do NOT use `rotateViewTransport` here.** It looks like the same fix and is not: that function
releases the old identity on the route being left (`terminal-stream-retarget.ts:172-207`) and runs
only when a direct route is ALREADY active. Using it at candidate time would park the view on Sync
before anything had proved the direct path, so a candidate that never earns a baseline leaves the
pane with no live view at all — strictly worse than the "stuck on Sync" this is meant to fix.

**The remaining cost was the identity source, not the mechanism.** `roost-client-core` has no
`uuid`/`rand` dependency and `is_terminal_uuid` is a pure FORMAT check, so the core must NOT
fabricate one: these ids go on the wire and into the worker's view records, and a deterministic
UUID-shaped name would later be mistaken for a random token. The id is minted by the HOST —
`Effect::MintTerminalViewId` asks, `ClientEvent::TerminalViewIdMinted` answers, and
`roost_web::platform::terminal_view_id::mint_view_id` is the one `crypto.randomUUID()` call the
document has. The core refuses whatever comes back that is not a v4 UUID, because the worker's own
`is_v2_uuid` admits versions 1-5 and a nil or time-based id is one this client never asked for.

**Guard** — `crates/roost-client-core/tests/direct_carrier_staging.rs`:
`the_minted_id_is_what_the_candidate_publishes_and_sync_keeps_its_own` (the candidate publishes the
HOST's id, and staging releases nothing on Sync), `a_minted_id_the_worker_would_refuse_abandons_the_attempt`
(a v1 is never published), `a_mint_that_collides_with_a_live_view_abandons_the_attempt` (two panes
may not end up on one handle), and `a_stale_or_repeated_mint_is_ignored` (a slow answer cannot
attach itself to a newer attempt, and a pane that has an id does not get a second).

**This is NOT the entry on the unimplemented WebRTC negotiation**, and it is not the entry on the
missing candidate staging. Those are two earlier links in the same chain and each has its own
entry; this one is reached only once a candidate is staged and a view actually arrives.

### The client's direct-carrier data plane folds cell frames and nothing else

**This is the umbrella entry for the whole direct-carrier chain, and it is the parent of the
three entries around it.** It is worth reading before spending time on any one of them.

**Symptom** — anything that needs a direct route to become a real route fails, on loopback and on
WebRTC alike, and never with a fault: the carrier admits, a candidate is staged, the pane keeps
painting from Sync, and the direct route never wins. The specific symptom depends on which link
is missing, which is why this looked like several unrelated bugs.

**Wrong** — treating `handle_direct_frame` as a cell-frame-only path
(`roost-client-core/src/handle_sync.rs:132-148`). It matches `SyncFrame::CellGrid` and
`SyncFrame::CellGridChunk` and sends EVERYTHING ELSE to a `_ =>` arm that logs "direct carrier
frame with no direct-carrier rule" at `debug`. And the frame really does arrive: the worker
answered the staged view command (`replies: 1` on a worker-side probe), the pump decoded it
(`DirectInbound::ViewState`), converted it (`inbound.rs:49` → `SyncFrame::ViewState`), and
dispatched it as `ClientEvent::DirectFrameReceived` — where it was then dropped.

So a direct carrier has no path for:
- **view state**, which is the acknowledgement that installs a view's stream and tells the replica
  its baseline is coming. Without it the candidate can never be told its view was accepted;
- **resync**, the repair a candidate asks for when its baseline does not arrive;
- **input results**, which is what fences a keystroke the direct link accepted.

**The three links under it, each with its own entry, in dependency order:**
1. the missing candidate STAGING (`RouteRegistry::stage` reachable only from
   `fold_into_candidate`) — FIXED, guard `crates/roost-client-core/tests/direct_carrier_staging.rs`;
2. view IDENTITY contention — v2 gives the candidate PROSPECTIVE view ids
   (`terminal-stream-promotion-candidate.ts:94` mints a fresh `crypto.randomUUID()` per view) so
   the candidate's records cannot collide with the Sync socket's; the ids are host-minted, so the
   core asks for one — FIXED;
3. this entry: the candidate had no view records and never received their acknowledgement.

**A claim in an earlier revision of this entry was wrong, and the wrongness is the useful part.**
It said the candidate "has no view records and never receives their acknowledgement, so
`baseline_ready` cannot be reached". `baseline_ready` is FRAME-driven and always was: folding a
complete, validated full is what sets it. What was true is one link earlier in the chain —
`frame_fold::valid_full` EXPLICITLY refuses a frame with no expected stream
(`crates/roost-client-core/src/terminal/frame_fold.rs`: an absent `expected_stream_id` returns false
before anything else is read, because admitting it would let any stream's baseline become the
canonical one). The expected stream is installed ONLY by an accepted view-state. So with the
direct `ViewState` dropped, every frame the candidate received was refused as a delta wearing a
full's flag, the baseline never completed, and `promote` never fired. The missing link is the
ACKNOWLEDGEMENT, not the readiness flag — which is why grepping for `baseline_ready` sends you to
the fold instead of to the view path.

**There was a SECOND, independent defect in the same function, and it is the reason this entry was
worth reading twice.** `fold_into_candidate` matched `promote`'s result as `Ok(_)`, which compiles,
logs "a direct candidate earned its baseline and is now the elected route", and DISCARDS the
`TerminalSession` the registry handed back. The route was elected and the replica was thrown away:
the canonical kept the old generation and the old grid, so `snapshot.route.active` said loopback
while the pane painted from a socket nobody was reading. Any fix that only taught the fold to
reach `baseline_ready` would still have left a route that is elected and unusable.

**Right** — port the candidate as a first-class thing rather than a bare replica: give it its own
prospective view records, fold `SyncFrame::ViewState` into the CANDIDATE by the id the candidate
minted, install the stream that answer names, and CONSUME the promoted replica — adopting its
prospective ids and releasing the old ones on the old transport in the same step (the shape
`terminal-stream-promotion-commit.ts` already has in v2). Order is the whole safety argument: new
ids published, old ids released only after the replica is swapped in.

**Guard** — `crates/roost-client-core/tests/direct_carrier_staging.rs`:
`a_view_answer_then_a_full_elects_the_route_and_retires_the_sync_view_afterwards` (the ordering,
end to end, including that a full arriving BEFORE the answer elects nothing),
`a_view_state_for_an_unpublished_id_changes_nothing` (the answer is correlated by the candidate's
own wire id, not the pane's), `the_same_pane_addresses_its_worker_by_the_minted_id_after_the_promotion`
(the pane's identity never changes; the wire id follows it), and
`a_frame_on_another_generation_does_not_elect`.

**This is NOT the entry on the unimplemented WebRTC negotiation.** That one is above the transport
(browser ⇄ coordinator ⇄ worker offer/answer); this one is below it, in what the CLIENT does with
what arrives. Both are required, and neither substitutes for the other.
### A dialog's body re-declaring the shell's `data-testid` makes every by-id assertion ambiguous

**Symptom** — a spec that names an overlay by test id fails on the COUNT rather than on what it is
asserting: `strict mode violation: getByTestId('command-palette') resolved to 2 elements`, or a
`toHaveCount(0)` after close that would also have failed for an overlay that never opened. The two
answers are listed side by side in the error, and they look like two overlays.

**Wrong** — the `Sheet`/`Dialog` carries `data-testid` for the surface AND the body inside it repeats
the same literal. v2 tagged the BODY (`CommandPaletteBody.tsx:124`), which was the only node then;
the Rust port moved the id onto the dialog and left the body tag in place, so both answer.

**Right** — ONE node answers to the id, and it is the dialog: it is the thing a spec asserts exists
when the surface is open and gone when it closes, and the `role="dialog"` + `aria-modal="true"`
node is what every accessibility query means by the overlay. A body inside it is selected by a
CHILD id (`command-palette-input`, `task-editor`), never by the surface's own name.

**Guard** — `crates/roost-web/tests/palette_overlay_lifecycle.rs`: `an_open_palette_is_exactly_one_node`
(one node, not two) and `closing_the_palette_removes_the_dialog_that_answered_to_its_test_id` (a
removal MUTATION for that element, not a rule that hides it — a node left in the tree is still in the
accessibility tree and still holds the focus-trap sentinels).

---

### An intent that reduces into the store is not a surface, and the reader gets no error for it

**Symptom** — a catalog row closes the overlay it was pressed in and nothing else happens: no form,
no editor, no card, no console line. The action looks broken and every layer that can be asked
reports success — the keypress ran, the intent dispatched, the store revision moved, the RPC was
never attempted. The only thing missing is a COMPONENT that reads the state the intent wrote.

**Wrong** — porting `ShellIntent::OpenQueueTaskDialog` and its `store().shell_dialogs.queue_task`
half, which is what the catalog row needs to be testable, and stopping there. v2's
`QueueTaskDialog` was a sibling line in `App.tsx`'s overlay group, so nothing in the store's shape
implies it exists: `open_queue_task` is a perfectly ordinary setter on a perfectly ordinary struct.

**Right** — every `ShellIntent` variant that WRITES shell dialog state has a host mounted beside
`CommandPalette` in `app::AuthorizedOverlays`, and the host renders nothing while the flag is false.
The intent is the door; the host is the surface; a port that ships one without the other is half a
feature, not a half-working one.

**Guard** — `crates/roost-web/tests/queue_task_dialog_mount.rs`: `the_queue_task_row_opens_the_editor_on_the_page`
(dispatching the palette's own intent puts ONE `task-editor` node and its working-directory field on
the page) and `closing_the_editor_takes_it_off_the_page`.

---

### A reopened pane stays on Sync beside a live loopback carrier that already admits its session

**Symptom** — after a layout apply or a navigation takes a pane away and gives it back, its session
reads `activeKind: "sync"`, `peerPhase: "idle"`, `candidateKind: null` for as long as anyone waits,
while the same worker's loopback carrier is live and its grant still names that session. Nothing
logs an error: no grant is minted, no carrier dials, nothing is staged. `terminal-peer-perf.spec.ts:7`
fails intermittently in its loopback scenario with "direct route unavailable".

**Wrong** — waiting for a grant refresh to restage it. The refresh never comes: the grant already
covers the session (`covers_demand`), so no mint is requested and no carrier widens, and
`handle_view_opened` only restaged a session that still had a staging candidate — the one the
navigation abandoned when it took the pane away.

**Right** — when a pane opens, stage its session on the live carrier that already admits it unless
that carrier already holds the session's route (`stage_opened_session` in
`crates/roost-client-core/src/handle_terminal/carriers.rs`; v2 `terminal-peer.ts` handleDemand →
stageCurrentConnection). A loopback route is never traded for a peer.

**Guard** — `crates/roost-client-core/tests/direct_carrier_staging.rs`:
`a_pane_opening_for_an_admitted_session_stages_it_on_the_live_carrier` (failed before the fix) and
`a_second_pane_of_an_elected_session_does_not_stage_it_again`.

---

### A key pressed right after the key that opened a menu lands on the wrong item

**Symptom** — ArrowDown on the sidebar machine trigger, then End at once: focus ends on the FIRST
machine, not the last, and End did nothing. It only shows under load, and only on the second key:
`terminal-delivery.spec.ts:130` at `expect(page.locator(":focus")).toContainText(secondWorker.label)`
receives the first item's text. Nothing logs.

**Wrong** — making the first focus attempt on an animation frame (or any later tick) because the
menu "has to mount first". Every key that arrives before that frame reaches the trigger, which has
no use for it, and the frame then focuses the edge the OPENING key asked for — the reader's next key
is lost. Waiting longer in the spec only hides it.

**Right** — v2 `contextMenuPrimitives.tsx` focusMenuEdge: attempt in a microtask, retry by frame
only when the menu was not yet in the document, and let the menu cancel a pending request on close
and on its next request (`cancelPendingFocus`). In Rust that is `focus_menu_edge` returning a
`MenuFocusRequest` (`crates/roost-web/src/components/context_menu{.rs,/dom.rs}`); the machine menu
also attempts from its `onmounted`, and Home/End that still beat the focus to the trigger retarget
the request (`machine_trigger_key_action` in `sidebar_new_terminal.rs`) instead of being dropped.

**Guard** — `crates/roost-web/tests/sidebar_logic.rs`:
`home_and_end_that_beat_the_menu_focus_to_the_trigger_still_pick_their_edge`.

---

### Two browsers on one WebRTC session paint the same output up to a quarter second apart

**Symptom** — `terminal-peer.spec.ts:97` (firefox-peer) fails about one run in ten at
`expectMarkersOnce` (`terminal-multiview-helpers.ts:336`, called from `:153`): the typing browser
painted the trusted key's `ACK:` and the second browser on the same session, read ~20 ms later,
has not. Nothing logs. Timed in-page, the second viewer's paint sits anywhere within ±250 ms of the
first's and holds the same offset for a whole run; `__smoke.input` on a peer route takes 150–280 ms
where loopback and Sync answer at once.

**Wrong** — reading it as Firefox load or as worker fan-out order. The worker hands one frame to
every peer sink in the same pass, and the native peer driver is woken on every send. Also wrong:
polling the second browser in the spec, which hides a latency v2 never had.

**Right** — the browser half drained the peer event sink only on the 250 ms peer tick
(`PEER_TICK_INTERVAL_MS`), so every arrived byte waited for that tick and each document's tick ran
at its own phase. `PeerEventSink::notify_on_record` (`crates/roost-web/src/platform/peer/events.rs`)
now rings the host after each recorded event and `pump::peer_lane::drain::install_tick` drains on
the next task, as `pump/socket.rs` (Sync) and `pump/carrier_dial.rs` (loopback) already did and as
v2 `terminal-peer-connection.ts` handled each `onmessage`. The tick keeps the deadlines and probes.

**Guard** — `crates/roost-web/src/platform/peer/events.rs`:
`the_host_is_rung_after_the_event_it_must_drain_is_queued`.

---

### A keeper outlives its deleted socket and ignores SIGTERM

**Symptom** — `roost-keeper` processes pile up long after their worker and data directory are gone:
one oracle session left 1155 of them (3.1 GB RSS, every one on a deleted
`/tmp/roost-terminal-system-*` socket) and the shells they held, where the Bun stack leaves none.
`kill -TERM` changes nothing: an idle keeper, or one serving a worker, is still alive seconds later,
so the worker's `keeper.restart_degraded`, which SIGTERMs the keeper it started, never gets the
clean keeper it asks for. The oracle's own teardown (`stopKeeper`, an authenticated v2 `Shutdown`)
now stops a Rust keeper at once — the keeper speaks v2's capability-bearing Hello
(`protocol/spec/keeper.md` § Hello) — and the socket check is the backstop for a keeper nobody shuts
down.

**Wrong** — SIGKILLing keepers from the harness or the worker, or treating a worker disconnect as a
shutdown: the keeper exists to outlive its worker. Also wrong: proving the SIGTERM handler with a
signal sent right after a listening probe connected — the signal usually lands while the keeper is
still serving that probe and is noticed on the way back to `accept`, so the test passes by luck.

**Right** — v2 `multiplexed-main.ts` shut down on `SIGTERM`, and when a 30 s `existsSync` found its
socket gone. The Rust handler only set a flag the daemon read between connections, while `accept`
blocked for good (std retries `EINTR`) and the connection loop never looked. `server::ExitWatch`
(`crates/roost-keeper/src/server/exit_watch.rs`) now carries the flag and the socket check;
`Server::accept_or_exit` waits in `poll` for at most 250 ms at a time and every connection turn polls
the watch, so either cause stops the keeper, which reaps its channels as every stop does.

**Guard** — `crates/roost-keeper/tests/keeper_daemon_exit.rs`:
`sigterm_stops_a_keeper_waiting_for_a_worker`,
`sigterm_stops_a_keeper_serving_a_worker_and_reaps_its_shells`,
`a_deleted_socket_stops_a_keeper_waiting_for_a_worker` and
`a_deleted_socket_ends_the_connection_being_served`. Real flow: no `roost-keeper` naming a test root
outlives a spec run; `stopKeeper` stops each one, and the 30 s check is the backstop.

---

### A worker link writes a reopened view's baseline ahead of the view-state that announces it

**Symptom** — a terminal reopened at the size it already had (back from a file preview, a re-reveal)
keeps its old screen until something prints. The browser logs `expecting a fresh baseline` and nothing
after it, the coordinator's `terminal_screen` is absent from `__smoke.terminalStreamProbe`, and the first
output afterwards logs `terminal.screen_resync` with "terminal delta arrived before a complete
baseline". In the oracle: `composer-mobile-keyboard.spec.ts:9` failing about 3 runs in 20 with
"terminal stream probe omitted a current worker/coordinator sequence".

**Wrong** — letting the link drain move the cell sink's frames onto the Terminal lane without first
admitting what the uplink already holds. The worker's view owner hands the uplink its view decision
(`TerminalViewState`) BEFORE it installs the new stream's baseline, but `link_serve`'s select is
`biased` with the cell wake ahead of `uplink.recv()`, so a drain the baseline woke wrote the cells while
the decision still sat in the channel. The coordinator's replica folds a full only against the stream it
was told to expect (`replica_admission.rs::accept_full` returns on a mismatch without a word), so the
baseline was dropped and the replica waited for its 10 s first-byte deadline or a delta's repair. A
geometry change hid it: the keeper resize took long enough for the select to serve the uplink first.

**Right** — cells enter the outbox only after every frame the uplink already holds:
`LinkLoop::move_cell_frames_into` admits `uplink.try_recv()` through `admit_uplink` before it moves
the cells, and the Control lane drains ahead of Terminal, so the wire order is the producer's order. v2
wrote both through at once, in call order.

**Guard** — `crates/roost-worker/src/runtime/link_loop/durable/tests.rs`:
`a_view_decision_queued_before_its_baseline_leaves_ahead_of_it`. Real flow:
`composer-mobile-keyboard.spec.ts:9 --repeat 20`.

### A release binary will not start on a fleet machine: "version `GLIBC_2.38' not found"

**Symptom** — a fleet machine runs the downloaded `roost` or `roost-keeper` and it exits before `main`:
`/lib64/libc.so.6: version 'GLIBC_2.38' not found (required by ./roost)` (or `GLIBC_2.39`; the keeper fails
too), `error while loading shared libraries: libssl.so.3`, or, on macOS, `Library not loaded:
/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib`. join.sh reports a fetched, digest-verified asset and then
the join fails. Observed on AlmaLinux 9 (glibc 2.34) running a binary linked on a glibc 2.39 host.

**Wrong** — building release binaries with the runner's own toolchain. The linker records, for each glibc
symbol, the newest version the runner's glibc offers: Rust's std alone references
`pidfd_spawnp@GLIBC_2.39`, and C dependencies compiled against glibc 2.38+ headers call
`__isoc23_strtol@GLIBC_2.38`. So the binary starts only on that glibc or newer. And web-push reaches
OpenSSL through `ece` and isahc's curl, which `openssl-sys` links dynamically from wherever the runner has
it: `libssl.so.3` on Linux (Debian 11 ships 1.1), Homebrew's prefix on macOS.

**Right** — the Linux release rows link through `cargo zigbuild --target <triple>.2.28`, which records
glibc 2.28's symbol versions whatever the runner has. The root manifest pins `openssl` with `vendored`, and
roost-coord declares it so feature unification compiles OpenSSL into the binary. A Linux binary then needs
only glibc's own libraries, and the one build starts on AlmaLinux 9 (glibc 2.34) and Debian 11 aarch64
(2.31).

**Guard** — `.github/workflows/release.yml`, build job, step "the binaries load nothing a fleet machine may
lack". It refuses a Linux binary with a NEEDED entry outside glibc and the gcc runtime, or a `GLIBC_`
version above its row's floor. It refuses a macOS binary that links anything outside `/usr/lib` and
`/System/Library`.

### Every `roost` command on a Mac fails with "unsupported host platform: macos"

**Symptom** — on macOS, `roost quickstart`, `roost worker`, `roost add-machine` and the rest print
`{"cmd":"…","error":"host.platform: unsupported host platform: unsupported host platform: macos"}` and stop.
Linux is unaffected. In CI: `worker_subcommand_boot.rs`
`the_worker_subcommand_boots_instead_of_refusing_to_share_a_runtime` fails on the macOS leg only.

**Wrong** — resolving the running host by parsing `std::env::consts::OS` with `HostPlatform::parse`. `parse`
reads the WIRE names, which follow Node's `process.platform`: `darwin`, `linux`, `win32`. Rust's std spells
macOS `macos`. The two vocabularies agree only on `linux`, so the parse worked on every Linux test host and
refused every Mac.

**Right** — `roost_host::supported_host_platform` takes the build target from `HostPlatform::current()`
(`cfg!(target_os)`), which cannot disagree with the binary it runs in. `HostPlatform::parse` stays the
reader for a name that arrives as data: a registry column, a deploy manifest, a wire field.

**Guard** — `crates/roost-cli/tests/worker_subcommand_boot.rs` on ci.yml's `macos-latest` leg. The real
flow is the `aarch64-apple-darwin` release binary: `roost quickstart --dry-run` on a Mac must print a plan,
not this refusal.

### `roost import-v2` on a fresh host: "could not be opened: … (code: 14) unable to open database file"

**Symptom** — the cutover's first write, `roost import-v2 --from …coordinator_v2.db`, run before `roost
quickstart` on a host that has never had v3, fails with `the v3 database
…/RoostCoordinatorV3/coordinator_v3.db could not be opened: sqlite: error returned from database: (code: 14)
unable to open database file`. The `--dry-run` just before it succeeds, so the failure reads as a different
problem.

**Wrong** — relying on SQLite's `create_if_missing` to create the target. It creates a missing FILE, not a
missing directory. On a fresh host nothing has created `RoostCoordinatorV3/` yet, because quickstart, which
does (`ensure_service_directories`), runs AFTER the import by design: the import has to land before
anything creates an account (`ensure_self_hosted_tenant`).

**Right** — `import_v2::apply` creates the target's directory with the same `create_dir_all`
quickstart's install uses, before the coordinator's own `db::open`.

**Guard** — `crates/roost-cli/tests/import_v2_copy.rs`:
`a_first_import_on_a_host_without_v3_creates_the_data_directory` drives the real `apply` into a directory
that does not exist. Without the fix it fails with the code-14 error above.

### A joined machine never appears: "https://… is not a coordinator this worker can dial"

**Symptom** — `roost join` reports "Joined", but the machine never shows up in the coordinator's roster.
Its worker restarts in a loop, and every attempt logs `{"cmd":"worker","error":"https://<host> is not a
coordinator this worker can dial"}`. Only a coordinator on the same machine (`http://127.0.0.1:4113`)
works.

**Wrong** — a worker that refuses `https`. `roost add-machine` only hands out an HTTPS origin
(`worker_dialable_origin`), and v2's workers dial `https://` front doors. But the boot-time Connect client
bailed on `https` ("configures no TLS connector"), and the link's `tokio-tungstenite` was built with no
TLS feature. So no machine could join over the internet, and every single-host test still passed.

**Right** — `crate::coordinator_tls` builds one rustls configuration: the ring provider, named rather
than taken from the process default, and Mozilla's roots compiled in. The Connect client takes it through
`HttpClient::with_tls`, and the link takes it through `Connector::Rustls` for `wss`. The configuration
sets no ALPN, so the WebSocket upgrade stays on HTTP/1.1.

**Guard** — `roost_worker::runtime::bootstrap_redeem::activation` test
`an_https_coordinator_gets_a_tls_client_rather_than_a_refusal`, and `coordinator_tls`'s ALPN test. The
real flow: a remote `roost join` appears in `roost status`'s worker list.

### A join replaced v2's `roost` CLI, or installed a v3 release into `RoostWorkerV2`

**Symptom** — after `join.sh` on a machine that runs v2, `roost` on that machine is suddenly v3's (v2's
CLI is gone from `~/.local/bin/roost`). Or the join prints `program:
…/.local/share/RoostWorkerV2/versions/<v>/bin/roost`, which is a v3 release inside v2's data directory.

**Wrong** — two writes to places the join does not own. join.sh installed the fetched pair into
`~/.local/bin` over whatever was there. That breaks the rule `self_link.rs` keeps: a regular file there
is refused. And `roost join` resolved its install directories from the ambient environment, while the
worker definition was resolved from a cleaned one. A join pasted into a terminal that v2 opened inherits
v2's `ROOST_WORKER_DATA_DIR`.

**Right** — join.sh keeps the pair in the temporary directory where it checked both digests, hands that
copy to `roost join`, and removes it once the join returns. `join::install_locations` resolves the
service and bin directories from `install_environment`, the same environment the definition comes from.

**Guard** — `crates/roost-cli/tests/join_script.rs`:
`a_join_that_fetches_leaves_a_v2_binary_at_the_self_link_location_alone` serves a fake release over
`file://`. `crates/roost-cli/tests/join_enrollment.rs`:
`a_join_from_a_v2_terminal_installs_where_its_definition_points_and_not_into_v2`.

### `roost join` from a release: "resolve the source commit: fatal: not a git repository"

**Symptom** — every join through `join.sh` stops right after the digests match, with
`{"cmd":"join","error":"resolve the source commit: fatal: not a git repository …"}`. Run from inside a
dirty checkout, it refuses with "uncommitted changes in working tree" instead.

**Wrong** — proving the joining build from the working directory's checkout. A release binary runs from
a staging directory, and the worker it installs reports the commit COMPILED into it. Any checkout that
happens to be the working directory names some other build.

**Right** — `join_identity::join_identity` enrols a binary as its compiled commit, and proves a checkout
(still refusing a dirty one) only for a `dev`-stamped build.

**Guard** — `crates/roost-cli/tests/join_enrollment.rs`:
`a_compiled_binary_enrols_as_its_own_commit_outside_any_checkout`.

### A machine's name ends in a line break: `roost status` prints its "— last seen" on the next line

**Symptom** — a machine installed by `roost quickstart`, which sets no `ROOST_WORKER_LABEL`, shows in
`roost status` as `✓ ovh1-8c32g` with ` — last seen …` wrapped onto the following line. Its
`workers.label` in the coordinator database is `"ovh1-8c32g\n"`. Machines added with `roost
add-machine` never show it, because their join command names them.

**Wrong** — taking the machine's own name raw. `/proc/sys/kernel/hostname` and `hostname(1)` both end
their answer with a newline, and registration re-sends the label on every boot, so a rename through the
API lasts only until the worker next restarts.

**Right** — `bootstrap_redeem::label::named` trims every source (the operator's label, the machine's
name, the shell's `HOSTNAME`), and a value that is empty once trimmed counts as unset.

**Guard** — `crates/roost-worker/src/runtime/bootstrap_redeem/label.rs`:
`a_machine_name_is_registered_without_the_newline_its_source_ends_with`.

### A test's `tracing` capture is empty only when the suite runs in parallel

**Symptom** — a test asserting on the `tracing` lines it captured fails with `left: 0` / `right: 1`, or sees
three events instead of four, under CI or a full `cargo test`, and passes run alone: e.g.
`agent_conversation_restore.rs:314` in `a_partly_delivered_resume_command_is_discarded_from_the_prompt` on
`ubuntu-latest`. `taskset -c 0,1` on the test binary reproduces it in roughly two runs of five.

**Wrong** — a per-test `tracing::subscriber::set_default` / `with_default` capture in a binary where a test
WITHOUT a capture reaches the same callsite. `tracing` caches each callsite's interest for the whole process
the first time any thread reaches it, and while one dispatcher is registered it asks the reaching thread's
default: the uncaptured thread answers through `NoSubscriber`, the callsite is cached `never`, and the
capture never sees it. A `rebuild_interest_cache()` after installing narrows the window but cannot close it,
because the uncaptured thread can reach the callsite first after the rebuild.

**Right** — every thread dispatches to one process-global router that files each event under the thread
that emitted it (`crates/roost-worker/tests/agent_prompt_support/log_capture.rs`: `enabled` is
unconditionally true, and the per-thread decision is made in `event`), or every test that can reach the
captured callsites is itself captured or serialized against the capture (`roost-observability`'s
`log::facade_callsites_exclusive`).

**Guard** — `crates/roost-worker/tests/agent_conversation_restore.rs`:
`a_partly_delivered_resume_command_is_discarded_from_the_prompt`, run beside
`a_proven_rejection_releases_the_reference_claim_and_an_ambiguous_one_keeps_it`, which reaches the same
`warn!` uncaptured.

### A restarted worker is linked and heartbeating but every pane on it reads "Machine offline"

**Symptom** — `roost status` shows the worker `✓ … last seen 2s ago`, the browser counts it out of
`N/5 workers` and labels its sessions "Machine offline — reopen to refresh"; the coordinator log has
`worker link: hello` for it and no `a worker snapshot crossed its readiness barrier`; the worker's
`session-event-outbox.sqlite` keeps `git`/`pr`/`ports` rows that never leave, and once 256 are queued it logs
`a durable row waits: the link cannot take it yet` in a hot loop.

**Wrong** — dropping pre-snapshot folder metadata with no ACK. The Rust worker journals `cwd`/`git`/`pr`/`ports`
in the same outbox as lifecycle rows and replays it one unacknowledged row at a time before its snapshot, so
the first metadata row emitted during boot holds the snapshot back forever and the generation never becomes
routable.

**Right** — the coordinator acknowledges and drops folder metadata that arrives before the snapshot barrier
(`worker_link::dispatch::is_folder_metadata`); the next change re-sends it.

**Guard** — `crates/roost-coord/tests/worker_frame_dispatch.rs`:
`folder_metadata_before_the_snapshot_is_acknowledged_and_never_written`.

### After a reboot the worker refuses to boot in a restart loop until the keeper socket is deleted by hand

**Symptom** — every `roost worker` start logs `the keeper endpoint held the connection and said nothing` and
then `keeper endpoint is held by a process that did not prove keeper identity … (HelloTimedOut)`; systemd
restarts it forever; `mux-keeper.sock` exists and `mux-keeper.pid` names a process that is not running.

**Wrong** — retrying a refused connect until the identity deadline. `connect` retries every error, including
the ECONNREFUSED a dead keeper's socket file returns, for 10 s; the probe's 5 s identity deadline fires first,
so "nothing listening" reads as a busy keeper and admission refuses rather than starting a fresh one.

**Right** — the probe dials with `roost_keeper::client::connect_unless_refused`, which returns `NotListening` on
the first refused connect and retries everything else exactly as `connect` does; `keeper_probe::probe` maps
`NotListening` to an empty probe, so `decide` starts fresh and `cleanup_endpoint` unlinks the stale file.

**Guard** — `crates/roost-worker/tests/keeper_probe_endpoint.rs`:
`a_published_socket_nothing_listens_on_is_an_empty_endpoint`, and
`a_published_socket_that_accepts_and_says_nothing_times_out`, which pins that a silent keeper still times out.
