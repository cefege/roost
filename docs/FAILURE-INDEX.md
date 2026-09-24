# Failure index

This file is the repo's institutional memory: every entry is a failure that actually shipped here, was
diagnosed, and was fixed — the wrong pattern is recorded next to the right one so it cannot be re-derived from
scratch.
It is grep-first, never read top-to-bottom: one `### ` heading per failure class, and the `**Symptom**` line
carries the report's own words, so grep this file for what the user said before you touch code.
`bun run lint` (`scripts/lint-roost.ts`) mechanically enforces the subset of entries whose `**Guard**` names a
rule string; the rest are pinned by the named test, and a `**Guard**` of `none` means nothing stops a regression
but this page.

---

## Solid store and component lifecycle

### Solid store write on a Record subtree silently no-ops

**Symptom** — "store doesn't update / sidebar doesn't reflect delete"

**Wrong** — `setStore("k", (prev) => newRecord)` on a Record subtree (silent no-op).

**Right** — per-key writes: `setStore("k", id, value)` / `setStore("k", id, undefined)`.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: Solid setStore(key, fn → newRecord) on a Record silently no-ops"`.

### SPA projector hand-mirrors the shared event fold

**Symptom** — "SPA store doesn't reflect a SessionEvent variant / coord and SPA projections disagree (stale channel)"

**Wrong** — re-implementing the event switch in `store/projector.ts` as a hand-mirror of `foldEvent` from
`@roost/shared/wire` (drifts — dropped `respawned`).

**Right** — `foldEventIntoStore` DELEGATES to shared `foldEvent` over the affected map slice, then diffs per-key
into the Solid store. No projector switch.

**Guard** — `apps/web/tests/store.test.ts` — `"projection agreement — foldEventIntoStore === shared foldAll"`
drives the REAL rootStore against `foldAll`.

### Reading props inside onCleanup throws on a torn-down node

**Symptom** — "Cannot read properties of null (reading 'X')" inside Solid cleanup

**Wrong** — reading `props.foo.bar` inside `onCleanup(() => …)` (reactive getter mid-cleanNode).

**Right** — capture `const stableX = props.foo.bar` at component body scope before `onCleanup`.

**Guard** — `scripts/lint-roost.ts` rule `"L11: never read props.* inside an onCleanup callback"`.

### A store proxy handed to a subscriber reads the POST-write value

**Symptom** — "agent status paints correctly but NO notification ever fires / a subscriber sees `previous.state === next.state` for a real transition"

**Wrong** — classify on the store value the projector read before writing, or "fix" the classifier to tolerate
self-transitions.

**Right** — snapshot any store value you hand to a subscriber. `setStore(path, obj)` over an existing object
node MERGES INTO THAT NODE (Solid `updatePath` → `mergeStoreNode`), so a proxy captured before the write reads
the POST-write value; `apps/web/src/store/agent-status.ts` publishes `{ ...current }`. Same trap for any
before/after diff taken off a Solid store.

**Guard** — `apps/web/tests/agent-status.test.ts` —
`"publishes a previous snapshot detached from the store node"`; `smoke/terminal/agent-status.spec.ts`.

---

## Sidebar, theming and design tokens

### An undefined token falls back to a hardcoded color

**Symptom** — "color shows as pitch black against new palette"

**Wrong** — `background: var(--bg-app, #111)` with `--bg-app` undefined → falls back to `#111`.

**Right** — every fallback must reference a defined token, OR the var must be declared in
`apps/web/src/styles/theme-vars.css`.

**Guard** — `scripts/lint-roost.ts` rules
`"L11: var(--<name>) is not declared in theme-vars.css or sidebar.css"` and
`"L11: hardcoded color fallback var(--x, #hex) — tokens are always defined; drop the fallback"`.

### Selected state derived from children instead of the URL

**Symptom** — "selected state lights everything coral"

**Wrong** — `data-selected={sessions().length > 0 ? "focused" : ""}`.

**Right** — `data-selected={useLocation().pathname.startsWith("/w/" + id) ? "focused" : ""}`.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: sidebar data-selected must be URL-driven, never sessions().length"`.

### A full-surface loading or error card reads as a failure, then "flips" to the real UI

**Symptom** — "I press the button and it shows an error, then it flips to the folder list". No
console error, no failed RPC: on loopback the card is invisible, and on a phone over a tailnet it is
the whole screen for over a second.

**Wrong** — a route-level `<Show>` that swaps the ENTIRE page between states, with an
`EmptyState icon="progress_activity"` as the loading arm. `progress_activity` has no spin rule in
`apps/web/src/styles`, so the "loading" arm is a static icon-plus-text card structurally identical
to the error arm beside it, and the wholesale swap to the loaded UI is the "flip". The same shape
turns every listing failure into an empty result: a `.catch` that only nulls the data renders
"Empty folder" for a directory that failed to read.

**Right** — mount the chrome once and switch only the content region. A surface owns an explicit
status (`loading | ready | error | offline`), loading paints skeleton rows shaped like the rows that
will replace them, and a failure keeps its own reader-facing copy plus a Retry that re-runs the
fetch. Denial replacements that a test pins by accessible name (here `browse-worker-unavailable`)
stay byte-identical and take the content region's place, never the page's.

**Guard** — `smoke/terminal/browse-picker.spec.ts` "folder picker keeps its chrome and names every
failure" (close/up/home/filter/New folder all visible the whole time, an invalid name reported
inline with `toHaveCount(0)` on the toast locator) and `smoke/terminal/worker-route-guards.spec.ts`,
which still pins the `Loading machine…` caption and the unavailable block's accessible name.

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

**Guard** — `smoke/terminal/browse-picker.spec.ts` "folder picker fits a phone": the `New folder`
control must be `toBeInViewport({ ratio: 1 })` and the machine label visible in an iPhone 15
context.

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

**Guard** — `smoke/terminal/workbench-shell-tab-strip.ts` `expectConnectedWorkbenchTabStrip`:
`widthsAboveFloor`, `fillerYieldsToTabs` and `widthsUniform` over six real sessions at 1024×768 (a
collapsed rail parks all six on the 68px floor and hands the leftover ~180px to the filler), with
`expectWorkbenchTabStripAtFloor` pinning the packed state.

### A sticky control inside a scroller is painted over by the content it must clear

**Symptom** — "existing tabs paint over the + button": a scrolled inactive tab merges into the
trailing button, the active tab is sliced at the button's left edge, and a dragged tab floats over it.

**Wrong** — parking the trailing control inside the scrolling rail as `position: sticky;
inset-inline-end: 0; z-index: 1` over an opaque background, and reserving its width with
`padding-inline-end` on the scroll content. Its z-index loses to any item that raises its own (a drag
lifts the grabbed tab), and the reserved padding inflates `scrollWidth`, which is the overflow signal
the chevron reads.

**Right** — make the control a flex SIBLING after the scroller. The scroller clips its content at its
own edge, so no scroll position, drag, or close animation can reach the control, and `scrollWidth`
stays honest.

**Guard** — `smoke/terminal/workbench-shell-tab-strip.ts`: `newTabFollowsRail`,
`newTabClearOfRail`, and `newTabIntersectsTabAt{Start,End}` at both scroll extremes, asserted in the
fit helper and again in `expectWorkbenchTabStripAtFloor`.

---

## Terminal history, rendering and scroll ownership

### Remounting the terminal on navigation destroys the session

**Symptom** — "terminal disconnects on nav / lost scrollback"

**Wrong** — `<Show when={activeSession()}>{(s) => <CellTerminal .../>}</Show>` (remount per nav).

**Right** — `<For each={openSessions()}>` deck in `apps/web/src/components/TerminalDeck.tsx` +
`visibility: visible↔hidden`. The deck host stays mounted for every MainPane screen so a `/file` or `/search`
visit never tears it down.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: CellTerminal must render inside <For> deck, never <Show> (remount on nav loses scrollback)"`;
`smoke/terminal/` — `"a /file round-trip keeps the deck warm and costs no snapshot"`.

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
coordinator replica; `terminal-stream.ts` applies the same rule to its
per-session browser replica. A gap latches one snapshot request. Chunked fulls
install atomically only after every viewport row occurs exactly once. Renderer
mount state is not part of the continuity proof.

**Guard** — `apps/shared/tests/cell-frame-chunks.test.ts`;
`apps/coord/tests/terminal-screen-hub.test.ts`;
`apps/web/tests/terminalStream.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`.

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
viewport-only (`normalizeCellGridFrame` in `apps/shared/src/cell/diff-grid.ts`),
so no authoritative history disagrees, and
`splicePage` in `apps/web/src/lib/scrollbackBackfill.ts` deliberately tolerates
a refused re-insert, so demand backfill never repairs those rows either.

**Right** — **painted history content is only ever the worker's own rows.** A
canonical viewport-only checkpoint reserves
`[previous.scrollbackTotal, frame.scrollbackTotal)` as an UNPAINTED gap
(`_extendScrollbackGap` in `apps/web/src/lib/cellRenderer.ts`, which already
holds that interval's exact pixel height) and lets the epoch-addressed,
worker-authoritative `SessionsGetScrollbackCells` backfill fill it on demand;
only a frame's own `scrollbackRows` / `scrollbackAppend` are ever painted.
Non-contiguous painted history is a first-class state
(`missingCellHistoryRanges` in `apps/web/src/lib/cellHistoryRanges.ts`), so a
reserved gap needs no inference to stand in for it. A matching history/head
boundary identifies a content-PROVED shift candidate, but global viewport reuse
is permitted only when `deltaViewportShift`
(`apps/shared/src/cell/diff-grid.ts`) receives the complete final viewport.
`applyDelta` and `foldCellDeltaBatch` then reuse that global shift; sparse
deltas patch only their worker-authored final coordinates and retain omitted
held rows, so a fixed footer cannot receive an older status generation.

**Guard** — `apps/shared/tests/cell-realcore.test.ts` —
`"partial-region scroll with a footer repaint preserves untouched rows"`;
`apps/shared/tests/cell-delta-batch.test.ts` —
`"preserves an untouched footer through a sparse partial-region batch"`;
`apps/web/tests/cellRenderer.reconcile.dom.test.ts` —
`"a partial-region scroll retains the fixed panel and worker history"`,
`"a batched partial-region scroll retains the fixed panel and latest status"`;
`apps/web/tests/cellRenderer.history.dom.test.ts` —
`"a checkpoint leaves the transitioned rows unpainted for authoritative backfill"`;
`smoke/terminal/terminal-render-main-repaint.spec.ts` —
`"a backgrounded inline TUI repaint never freezes a stale generation into history"`.

### Alt-screen wallpaper of stale text after a worker restart

**Symptom** — "after worker restart an alternate-screen session shows wallpaper of stale text + overlapping/parallel lines"

**Wrong** — `resume()` rebuilds an empty wtermCore + records alternate-screen state, but the snapshot taken from
that rebuilt core reads `core.usingAltScreen()` (false on an empty core) → the fresh snapshot reports
main-screen → live alt redraws land in main-screen.

**Right** — **prime the rebuilt core's alt state** in `resume()` (`apps/worker/src/session-resume.ts`) whenever
the retained session state says it was using the alternate screen: `wtermCore.writeRaw(ALT_ENTER_SEQS[0])` after
the core is created so `core.usingAltScreen()` matches the retained state. NOT a forced SIGWINCH (TUIs repaint
alt but do not necessarily re-send `?1049h`).

**Guard** — `apps/worker/tests/session-manager-altmode.test.ts`.

### History gone after a worker restart because the keeper retained none

**Symptom** — "history GONE after worker restart + browser refresh; pane freezes / seq-epoch reset / 'new browser fixes it'"

**Wrong** — `resume()` rebuilds `scrollback:new Uint8Array(0), head_seq:0` because the keeper retained NO
per-channel history → the SPA's persisted lastSeq goes stale-high → seq-epoch reset, history unrecoverable.

**Right** — **the keeper retains a per-channel `outRing`+`headSeq`**
(`apps/worker/src/keeper/keeper-frame-handler.ts`, advanced in the same callback that broadcasts so it matches
the worker count); `GetHistory`/`GetHistoryResp` are represented by the authenticated
`KeeperContractV1` feature sets. Boot adopts a protocol-compatible survivor, but an
incompatible survivor blocks replacement while coordinator sessions or keeper bindings
remain live. `resume()` re-reads via the pool's history call
(`apps/worker/src/keeper/keeper-pool-channels.ts`) and seeds
`scrollback`+`head_seq`; only a proven-empty incompatible keeper may be replaced.

**Guard** — `apps/worker/tests/keeper-history-resume.test.ts`.

### No scrollbar in the terminal — the container CSS, not the core

**Symptom** — "no scroll bar / mouse wheel does nothing in terminal / can't scroll up to see history"

**Wrong** — switch to alternative terminal cores / upstream the core / patch the WASM "because
getScrollbackCount returned 0 in my synthetic test".

**Right** — **`.wterm { overflow-y: auto; overflow-x: hidden; }` in `apps/web/src/styles/sidebar.css`.** The
core DOES populate scrollback and the renderer DOES emit scrollback row DOM; the only thing missing was the
container CSS that lets those rows be scrolled. A synthetic test reporting zero scrollback usually means the
renderer hasn't painted yet (rAF does not fire in background tabs) — force a render before checking. DO NOT
switch terminal cores; the bug is one CSS rule.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: .wterm must keep overflow-y: auto (scrollback rows clip otherwise — do NOT switch cores)"`.

### The whole screen rubber-bands when a touch drag runs out of scroll

**Symptom** — "on mobile, dragging at the bottom drags/bounces the whole screen",
"the page gets pushed while I scroll the terminal", "pull-to-refresh fires inside
the app".

**Wrong** — a JS rubber-band, a `touchmove` `preventDefault()` race, or
per-component scroll locks. `apps/web/src/lib/overscroll.ts` was exactly that and
was deleted: a handler that claims the edge after the browser already started
scrolling always loses, because later `preventDefault()` on a started scroll is
ignored.

**Right** — **declare the policy once in `apps/web/index.html`'s base style:
`* { overscroll-behavior-y: none; }` plus `html, body { overflow: hidden; }`.**
The first kills chaining and the local elastic edge for every scroller
(`.wterm` included); the second denies the document a scroll range at all
(`height: 100%` resolves against the large viewport while `.workbench-shell` is
sized in `svh`). For a gesture the terminal application owns, `touch-action`
flips to `none` on the pane display (`CellTerminal.tsx`, keyed on
`mouseGesturesForwarded`) so the browser never starts a pan to begin with.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: index.html base style must keep * { overscroll-behavior-y: none } + html,body { overflow: hidden } (else mobile drags the page)"`,
plus `smoke/terminal/workbench-shell-compact.spec.ts` (document has no scroll
range; root and terminal display compute `overscroll-behavior-y: none`) and
`smoke/terminal/composer-mobile-keyboard.spec.ts` (`touch-action` flips to `none`
while forwarding, back to `pan-y` when it is toggled off).

### A list refuses to scroll while the cursor sits on a row's clipped label

**Symptom** — "the agents list only scrolls from some parts of a row",
"the wheel does nothing over the row title but works 20px lower", a wheel event
that reaches the row (`defaultPrevented` false) with no `scroll` event on the
list.

**Wrong** — blame scroll latching, relax the gate that wheels at a panel's
centre, or narrow `* { overscroll-behavior-y: none; }` in
`apps/web/index.html` (the entry above owns that policy, and `lint-roost`'s L11
rule pins it). Nothing about the scroller is broken: `scrollTop` writes still
land, and the same wheel scrolls from a neighbouring pixel.

**Right** — **a box that only clips text must not swallow the gesture.**
`overflow: hidden` makes it a scroll container, the global policy then denies
that container scroll chaining, and a wheel landing on the text is consumed by
a container with no scroll range instead of reaching the list. Each text clip a
pointer can land on inside a scroller opts chaining back in with
`overscroll-behavior-y: auto` (`.md-list-row__headline` in
`apps/web/src/components/Settings/md/tokens.css`;
`.workbench-sidebar-agents__group-name`, `__group-server` and
`.agent-status-rollup` in `apps/web/src/styles/workbench-sidebar.css`). It has
no scroll range of its own, so chaining there can only reach the list, and the
list's own `none` still stops the chain from reaching the document. Rows that
cover their whole area with a hit target (`.df-row__primary` in the folders
list) never showed the defect, which is why it surfaced only in the agents
list.

**Guard** — `smoke/terminal/workbench-shell.spec.ts`
`"desktop sidebar keeps Folders and Agents independently navigable"`: it wheels
at each panel's centre, which lands on an agent row's headline, and fails with
`scrollTop` stuck at 0 without the opt-in.

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

**Guard** — `apps/coord/tests/terminal-view-hub.test.ts`;
`apps/coord/tests/terminal-view-registry-membership.test.ts`;
`apps/worker/tests/terminal-stream-state.test.ts`;
`apps/shared/tests/wterm-resize-in-place.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`.

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
computes the minimum.** `minimumTerminalGeometry` (`@roost/shared/viewport`) is
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

**Guard** — `apps/coord/tests/terminal-view-registry-membership.test.ts`;
`apps/coord/tests/diag-snapshot-session-viewers.test.ts`;
`apps/coord/tests/worker-respawn-geometry.test.ts`;
`apps/web/tests/cellTerminalViewport.parkGrace.test.ts`;
`apps/web/tests/wtermSizeEstimate.dom.test.ts`;
`smoke/terminal/terminal-multiview-geometry.spec.ts`.

### Attach/reveal cost proportional to scrollback depth

**Symptom** — "attach/tab-switch/resize slow proportional to scrollback depth / long sessions stall seconds on pull-in while fresh ones are instant"

**Wrong** — ship the ENTIRE retained scrollback (≤10k rows) in every full cell frame — O(history) snapshot work
inside the claim, one MB-scale proto blob head-of-line-blocking the Sync stream, O(history) decode+DOM on the
SPA before first paint; or "fix" it by racing/timeouting the history away (trading history for latency is
forbidden).

**Right** — **history is PULLED on demand and never shipped wholesale.**
`apps/web/src/lib/scrollbackBackfill.ts` pulls ranges in chunks via `SessionsGetScrollbackCells` (coord relay →
worker `handleGetScrollbackCells` in `apps/worker/src/browser-command-terminal.ts`, serving
`readScrollbackRangeCells` from `apps/shared/src/cell/grid-to-cells.ts`) and `prependScrollback` in
`apps/web/src/lib/cellRenderer.ts` splices above the reader. At a literal bottom the renderer pins the new
bottom; otherwise it leaves `scrollTop` untouched. History ALWAYS arrives — only its timing is lazy. The
intermediate form (a fixed 250-row tail in every full frame plus a `mergeFullFrame` tail merge) is RETIRED: full
frames are now viewport-only, see the epoch-addressed entry below.

**Guard** — `apps/worker/tests/scrollback-cells-backfill.test.ts`; `apps/web/tests/scrollbackBackfill.test.ts`;
`apps/web/tests/` renderer DOM suite — `"CellGridRenderer DOM — viewport-only frames + backfill"`.

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
`apps/web/src/lib/scrollbackBackfill.ts`, and they only work together: (1) the trigger window is widened upward
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
page geometry itself is pure and lives apart in `apps/web/src/lib/scrollbackDemandBounds.ts`; each new pager
state (`scrollback.demand_coalesced`, `demand_rearmed`, `demand_retry_deferred`, `demand_retry_woke`) emits one
`diag()` line, the deferred pair naming the state that used to leave a visible gap unpainted. Unpainted
placeholders also stop reading as empty — `.cell-grid .cell-sb-gap` / `.cell-sb-spacer` in
`apps/web/src/styles/sidebar.css` paint a row-pitch skeleton (paint-only; those elements' inline pixel heights
are what every scroll position is derived from, so never give them geometry). The head spacer drops that
texture only while it lies ENTIRELY below a proven retention floor (`setHistoryFloor` →
`data-history-floor`, re-derived in `_syncSpacer`): those rows are gone, not loading, and a pending sheet over
them would read as a load that never ends — but a floor proven for an interior gap must not suppress head rows
that are still pageable.

**Guard** — `smoke/terminal/terminal-history-readahead.spec.ts` (real stack: the first wheel step crosses
unpainted history and costs exactly the chain depth the pager's own constants predict, and every step inside a
pre-paid count DERIVED from the measured row height and pane size is painted at zero demand RPCs);
`apps/web/tests/scrollbackBackfill.bounds.test.ts` — the page never collapses to the sliver the window exposed,
whatever the interval's shape; `apps/web/tests/scrollbackBackfill.test.ts` —
`"a wheel step pre-pays the rows above the viewport and the next step is free"`,
`"scrolls during a wave add no request, one coalesce line, and one demand after"`,
`"a page that cannot splice retries bounded and stays armed for the reader"`,
`"the reader's own rows paint when one page would span the painted base"`,
`"a spent budget keeps re-deriving on the retry cadence, one wave per interval"`,
`"suspend and dispose cancel the deferred re-arm"`.

### Scroll position lurches — many writers of scrollTop

**Symptom** — "terminal scrollback jumps around / view lurches while scrolling up / lands mid-history after a tab switch / drifts off the bottom after vim/less/claude exits"

**Wrong** — row-space or pixel scroll ownership: intent/anchor state, distance compensation, scroll-event
classification, resize/reveal correction, or a jump-to-bottom control.

**Right** — **one pre-mutation capture plus ONE conditional writer.** `CellGridRenderer`
(`apps/web/src/lib/cellRenderer.ts`) captures `_atBottomOrOwnedPlacement()` before a painted-height
mutation; only `_pinToBottom(shouldPin)` may assign `scrollTop`, and only when that captured value was
true. The capture is the FOLLOW BAND (`followsScrollBottom`, two rows of slack — see the follow-band
entry below), never a widened `atBottom()`: `atBottom()` itself stays exact and is what the clamp and
settle paths keep asking. Non-bottom mutations never write position. The mutable append tail is
`overflow-anchor:none` so Chromium does not follow it when the reader is one pixel above bottom;
completed blocks are anchors, and the tail is restored before a backfill prepend so native anchoring
preserves the reader's row. Never restore intent state, add reveal correction, or a jump-to-bottom
control. This single-writer invariant is about POSITION and
presumes the scroll SPACE is truthful — the spacer entry below is what makes it so.

**Guard** — `apps/web/tests/` renderer DOM suite —
`"a non-bottom backfill prepend performs no application scroll write"`,
`"only the mutable tail is excluded from browser anchoring"`,
`"unchanged and fully clamped pins leave no stale scroll ownership"`,
`"a coalesced pin retargets once, then the next native scroll reads"`.

### A parked pane paints at a lying box size

**Symptom** — "tab switch shows stale terminal content / a returned-to pane sits above the live bottom and never follows output again / bottom-follow works foreground but dies after a park"

**Wrong** — latch the bottom in intent state, correct scroll at reveal, add an `atBottom()` tolerance (the
CLAMP predicate stays exact; slack lives in the separate follow-band predicate, see the follow-band entry
below), or defer
`_pinToBottom` to a rAF — all forbidden by the entry above; equally wrong: leave a parked pane painting at a
DIFFERENT box size (the old fixed 800×600 park) so its scroll maximum moves under it.

**Right** — **a pane that keeps painting off-screen must have TRUTHFUL geometry, not a corrected scroll
position.** Three invariants, all measured live: (1) the deck parks a pane at its own leaf's rect
(`parkSizeBySession` in `apps/web/src/components/TerminalDeck.tsx`) so `clientHeight` is identical parked vs
revealed; (2) block placeholders are a BARE length, never `contain-intrinsic-size: auto <len>` — `auto` makes
the browser reuse a block's LAST RENDERED size, so a block that grows while skipped understates `scrollHeight`
until it materializes; (3) the OPEN tail block opts out of `content-visibility` until it seals — a skipped
subtree's intrinsic size is re-evaluated at rendering-lifecycle time, not on append, so appending into a locked
tail leaves `scrollHeight` stale and the pre-mutation bottom check reads a bottom that no longer exists. Sealed
blocks stay skipped, so deep-history layout stays O(blocks). Measured: a 250-row block remembered at 29 rows
reported 487.11px instead of 4199.22px; revealing it grew `scrollHeight` by exactly that 3712px.

**Guard** — `apps/web/tests/` renderer DOM suite —
``"the placeholder is a bare length — never the self-correcting `auto` form"``,
`"only the OPEN tail block opts out of content-visibility; sealing restores it"`,
`"at-bottom reader follows a box grow onto the new bottom"`,
`"a live old-bottom anchor follows a box shrink with exactly one pin"`.

### A box grow under a parked reader removes the last scroll event

**Symptom** — "terminal died after the window/pane/composer changed size and never streamed again / scrolling
back to the bottom does not restart it / only a reload fixes it", with `at_bottom` already TRUE and
`reconcile_block_reason=reader_pending_frame` while the canonical seq keeps climbing.

**Wrong** — resume a parked reader from `ResizeObserver` only when `_readerReason === "native_scroll"`. Real
wheel and touch gestures park as `"wheel"` / `"touch"` (`apps/web/src/lib/terminalMouseForwarding.ts`), so that
gate was dead for every real gesture. Equally wrong here: an `atBottom()` tolerance or any reveal/resize scroll
correction — the geometry was measured INTEGRAL in 184 real layouts at four device-pixel ratios (the true clamp
equals `scrollHeight - clientHeight` exactly), so the clamp predicate was never the defect (what a reader near
the tail is ALLOWED to do is a separate policy — see the follow-band entry below). Equally wrong: gating the
bottom-clamp settle on the `native_scroll` reason alone. The scroll handler records `native_scroll` for a real
movement and `terminalMouseForwarding.ts` then UPGRADES that park to the gesture's own `wheel`/`touch` reason,
so the settle armed for the next frame no longer matches its own park; if layout clamps that park onto the
exact bottom, no further scroll event exists and nothing resumes it. Also wrong: letting a paint hold swallow
the repair — `noteBoxResize` advances `_lastBoxH` before it returns, so the ResizeObserver cannot retry, and
`_resumeLive` refuses under a hold, so a wheel park plus a link hover plus a zero-range grow consumed the
pane's only resume while the later hold release refused too, because release resumed a `selection` park only.

**Right** — **a park must be exitable by an event the pane can still deliver.** A parked reader freezes the
DOM, so a grow past the frozen content leaves `scrollHeight === clientHeight`: the box can never fire another
scroll event, `handleScroll()`'s bottom resume (`apps/web/src/lib/cellRenderer.ts`) is unreachable, and
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

**Guard** — `apps/web/tests/cellRenderer.geometry.dom.test.ts` —
`"a wheel-parked reader resumes when a box grow leaves no scroll range"` and its three siblings;
`smoke/terminal/terminal-render-box-grow-resume.spec.ts` (real wheel park + viewport grow must repaint);
`apps/web/tests/cellRenderer.nativeScrollSettle.dom.test.ts` —
`"a wheel park clamped to the bottom settles without a second scroll event"`;
`apps/web/tests/cellRenderer.append.dom.test.ts` —
`"a hold release resumes a wheel park whose box lost its scroll range"`, with
`"a hold release leaves a find park that can still reach its anchor"` as the refusal control.

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
`apps/web/src/lib/cellRendererPresentation.ts` define one band around the clamp, and only four call sites
use it: `handleScroll()`'s park decision, the pin capture `_atBottomOrOwnedPlacement()`, the backfill
demand gate (`scrollbackBackfill.onUserScroll`, a band follower must not start paging history), and the
band settle. A reader inside the band is riding the tail, so the pane keeps painting and keeps pinning;
one wheel notch (~100px) is outside it and still parks. A park that comes to REST inside the band resumes
through `CellGridRenderer.settleFollowBand()`, armed `BOTTOM_FOLLOW_SETTLE_MS` (180ms) after the last
scroll event by the pane's own scroll listener (`apps/web/src/components/cell-terminal-renderer.ts`) and
also by frame arrival (see the entry below), so the resume never runs mid-gesture. A hold release also
resumes a band-following
position-only park, because the hold swallowed the only scroll event that could. `at_bottom` in the
presentation snapshot keeps its exact meaning; `follows_bottom` is the band value beside it.

**Guard** — `apps/web/tests/cellRenderer.readerIntent.dom.test.ts` —
`"a live reader inside the follow band keeps following the tail"`;
`apps/web/tests/cellRenderer.nativeScrollSettle.dom.test.ts` —
`"a wheel park resting inside the follow band resumes on the settle"`, with
`"a park beyond the follow band survives the settle"` and
`"a find park inside the follow band keeps its anchor through the settle"` as the refusal controls;
`apps/web/tests/cellRenderer.append.dom.test.ts` —
`"a hold release resumes a bottom-following wheel park that kept its range"`;
`smoke/terminal/terminal-follow-band.spec.ts` —
`"a follow-band reader keeps streaming, self-resumes, and still parks past the band"` (real trusted wheel:
an in-band flick self-resumes, a 1200px gesture still parks and still swallows output).

### A wheel park created after the gesture's last scroll event never resumes

**Symptom** — "I am sitting near the bottom, output stops, the dot goes amber and stays there; typing is the
only thing that brings it back" — `reconcile_block_reason=reader_pending_frame` with `follows_bottom` TRUE,
`at_bottom` FALSE, `reader_reason` `wheel` or `touch`, and canonical climbing away from the DOM forever.

**Wrong** — arming the band settle from the pane's scroll listener ALONE. `enterReadingForNativeScroll`
(`apps/web/src/lib/terminalMouseForwarding.ts`) parks from a capture-phase, non-passive wheel/touchmove
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

**Guard** — `apps/web/tests/cellRenderer.nativeScrollSettle.dom.test.ts` —
`"a band rest parked with no scroll event recruits the settle on a frame"` (and asserts zero `scrollTop`
writes at frame arrival),
`"a stream of frames over a band rest keeps exactly one settle window"`, with
`"a park beyond the follow band recruits no settle window"` and
`"a park on the exact clamp resumes on the frame with no settle window"` as the boundary controls;
`smoke/terminal/terminal-follow-band.spec.ts` for the real-flow band behaviour.

### A find park swallows the scroll that returns the pane to the bottom

**Symptom** — "used find, closed the find bar, scrolled back to the bottom, and the terminal is frozen until I
type"

**Wrong** — treating EVERY scroll event as sacred to the find anchor: `handleScroll()` capturing the anchor and
returning before the at-bottom resume whenever the reason is `find`. `closeFind()`
(`apps/web/src/lib/terminalFindController.ts`) only clears highlights and query state — it never resumes the
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

**Guard** — `apps/web/tests/cellRenderer.findPark.dom.test.ts` —
`"a user scroll to the exact bottom resumes a find park"`,
`"a renderer-owned write that lands at the bottom keeps the find park"`,
`"a find park survives a scroll that does not reach the bottom"`,
`"a box-grow clamp onto the bottom keeps a find park"`,
`"a clamp that leaves no scroll range resumes a find park"`, and
`"a gesture after a clamp still resumes a find park"`, which pins the one-shot property — making the record
conditional on `clamped` turns it red;
`apps/web/tests/cellRenderer.nativeScrollSettle.dom.test.ts` —
`"a wheel park clamped onto the bottom by a box grow resumes"` for the position-only side.

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

**Guard** — `apps/web/tests/cellRenderer.findPark.dom.test.ts` —
`"closing the find bar ends the park without moving or painting"` and
`"a dismissed find park follows a box grow its anchor would have refused"`, both driving the real
`createTerminalFind` against a real renderer.

### A paint hold armed on an edge outlives the listener that would clear it

**Symptom** — "terminal never paints again after a UI layout change / typing reaches the PTY but the grid is
frozen", with `hold_mask {selection: true}` while nothing is selected anywhere on the page, or
`hold_mask {link: true}` after a modifier keyup that was delivered somewhere else, and a scroll to the exact
bottom returning `{reconciled:false, anchorChanged:false}`.

**Wrong** — arm `RENDERER_HOLD_SELECTION` edge-only from the document `selectionchange` listener and re-attach
that listener without re-deriving the hold. The pane detaches its global listeners for the whole of a withdraw
(`apps/web/src/components/cell-terminal-interactions.ts`), and a transient layout gap routes through
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
(`apps/web/src/components/terminal-links.ts`), and that predicate must be TOTAL — an event with no modifier
fields must read as "not held", never `undefined`, or the hold ends up neither armed nor disarmed. Re-entering
a pane with nothing held can no longer revive a hold from a dead edge.

**Guard** — `apps/web/tests/cellTerminalVisibility.test.ts` —
`"a selection dropped while the pane's listeners are detached stops holding paint"`,
`"a selection still live when the listeners re-attach keeps paint held"`;
`apps/web/tests/terminal-links.dom.test.ts` — the modifier-level cases, including
re-entry with no modifier held.

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

**Guard** — `apps/web/tests/cellTerminalPresentation.test.ts` —
`"re-arms the DOM target after a reader hold declined the deadline"`, with
`"preserves reader holds and pointer gestures through the DOM deadline"` as the refusal control proving a
hold still blocks escalation.

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

**Guard** — `apps/web/tests/` renderer DOM suite — `"the spacer reserves the unpainted history"`,
`"a head page shrinks the spacer by exactly the rows it adds"`,
`"an eviction grows the spacer by exactly the rows it drops"`,
`"renderFull reserves the incoming spacer BEFORE wiping painted history"`.

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

**Guard** — `apps/web/tests/terminalStream.test.ts`;
`apps/web/tests/cellRenderer.reconcile.dom.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`;
`smoke/terminal/terminal-render-resume.spec.ts`.

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

**Guard** — `smoke/terminal/` — `"deck switch to a stale deep-history pane lands at the live bottom instantly"`,
`"a pane revealed after the window shrank is still at the bottom"`,
`"a /file round-trip keeps the deck warm and costs no snapshot"`.

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
(`apps/coord/src/connect/handlers-sessions-scrollback.ts` relays it); the worker
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

**Guard** — `smoke/terminal/` — `"deep-history attach/reveal paints the live tail until history is requested"`,
`"long hidden deep-history resume paints the current viewport before history"`;
`apps/web/tests/scrollbackBackfill.test.ts`; `apps/web/tests/` renderer DOM suite —
`"viewport-only full reserves depth; explicit pages fill the seam"`.

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
  selection+link hold mask (`apps/web/src/lib/cellRenderer.ts`); passive output and composer drafting never
  cancel a reader, one admitted local keystroke calls `prepareLiveInteraction()` (clear holds + adopt
  reader-pending frame + re-pin bottom as ONE transition), and park/`pagehide`/unmount ENDS the reading interval
  so a revealed pane presents the newest canonical frame.

- (b) `terminal-stream.ts` owns one per-session browser replica and stable view
  handles. `CellTerminal` only measures, publishes active/inactive geometry,
  forwards attributed input and attaches a renderer. Detach never destroys the
  baseline; a reconnect replays desired views and resumes from a full snapshot.

- (c) `TerminalViewHub` is the only membership/SCD owner, and membership is not
  geometry. It independently minimizes columns and rows across the views that
  are actually looking (`minimumTerminalGeometry`, `@roost/shared/viewport`).
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
(`apps/web/src/lib/terminalDiagSnapshot.ts`), never a screenshot.

**Guard** — `apps/shared/tests/cell-frame-chunks.test.ts`;
`apps/worker/tests/terminal-stream-state.test.ts`;
`apps/coord/tests/terminal-view-hub.test.ts`;
`apps/coord/tests/terminal-view-registry-membership.test.ts`;
`apps/coord/tests/terminal-screen-hub.test.ts`;
`apps/web/tests/terminalStream.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`.

### A busy session restarts its own baseline forever while chunks assemble

**Symptom** — "ordinary frame interrupted chunk assembly / attaching to a busy terminal never
finishes — the coordinator keeps requesting snapshots and `terminal.screen_resync` loops"

**Wrong** — aborting the in-flight chunked full and latching a resync because ANY ordinary frame
arrived mid-assembly. A session emitting deltas faster than its multi-megabyte baseline chunks land
restarts the transfer on every delta: the worker builds another full, the next delta interrupts it
again, and attach never completes.

**Right** — park ordinary deltas in a per-session bounded hold (`TerminalAssemblyHold` in
`apps/coord/src/connect/terminal-screen-hub-state.ts`: 512 frames / 4 MiB, mirroring the Sync v2
delta-tail caps) while chunks assemble. When the assembled full installs, replay only held deltas
whose base_seq extends the new baseline — earlier ones are already contained in that full — through
the ordinary delta fold. Overflow, any other interruption, invalidation, or a minted stream clears
the hold and falls back to the single-resync latch; an ordinary FULL still supersedes the partial
outright without a resync.

**Guard** — `apps/coord/tests/terminal-screen-hub-chunks.test.ts` —
`"holds live deltas during chunk assembly and folds them like an uninterrupted run"`,
`"falls back to the resync latch when the delta hold overflows"`.

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

**Guard** — `apps/coord/tests/terminal-screen-hub-snapshot.test.ts`, `describe("TerminalScreenHub baseline
watchdog")` — escalation to a fresh stream across both ladder attempts; zero requests when the baseline
lands in time; a re-mint replacing the superseded deadline while the stale callback stays inert; a chunked
transfer in flight at the deadline left to the chunk stall timer.

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

**Guard** — `apps/web/tests/terminalStreamLifecycle.test.ts` —
`"retries an active view whose socket write never reached the transport"`,
`"republishes a refused active view inside one coordinator lease window"`, with the hidden-page case as the
refusal control.

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

**Guard** — `apps/web/tests/terminalStream.test.ts` —
`"re-arms the idle probe when a liveness challenge cannot be published"`,
`"arms a proof deadline for a latch whose delta cleared its latch timestamp"`, alongside
`"coalesces same-generation repairs across concurrent view renewals"`;
`apps/web/tests/terminalStreamProbeEpisode.test.ts` —
`"reports a rearm episode again after liveness retirement"`, which drives `Date.now()` explicitly so the
two episodes straddle `SIGNAL_COOLDOWN_MS`: a report the cooldown suppressed would green the case with the
retirement reset removed.

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

**Guard** — `apps/web/tests/offlineWatch.test.ts` —
`"a pane that painted, then lost its view, is re-claimed then accused"`, with
`"a quiet pane with a healthy view is never re-claimed, however long"` as the false-positive control and
`"a frame clears offline immediately even while the view reads detached"` for self-correction.

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

**Guard** — `apps/web/tests/cellTerminalVisibility.test.ts` —
`"a suspension whose restore never runs stops holding paint once its range is gone"`, with the still-live
suspension case as the refusal control that keeps the composer contract, and both pre-existing
level-derived cases untouched.

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

**Guard** — `apps/worker/tests/terminal-stream-core-trap.test.ts` —
`"a trapped resize releases the emission gate it can never lift"`,
`"a generation minted after a core trap inherits no dead capture"`, with
`"a trapped core still refuses frames for the generation it trapped"` as the fail-closed control.

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

**Guard** — `apps/coord/tests/terminal-view-hub-worker.test.ts` — the worker-generation recovery case plus
`"never redrives an invalid worker request from heartbeat or route events"`, which is the control proving a
protocol violation did not become a retry loop.

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
stream desire reaches it (`apps/worker/src/session-core-reprove.ts`, spliced into the `!state.coreValid`
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

**Guard** — `apps/worker/tests/terminal-stream-core-trap.test.ts` —
`"a fail-closed core is re-proved from keeper history on the next stream desire"` with
`"a core the keeper cannot re-prove stays fail-closed"` as the refusal control;
`apps/worker/tests/terminal-view-owner.test.ts` —
`"a trapped core re-proves itself on the desire the trap triggers"` (the desire COUNT is what proves it is
not a loop) with `"a trap the keeper cannot re-prove stays fail-closed and desires nothing more"`;
`apps/web/tests/sync-bootstrap-hydration.test.ts` —
`"a hydration that never settles is cancelled and retried"`;
`smoke/terminal/terminal-view-reap.spec.ts` — the real-flow reaped-then-newcomer path.

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
(`apps/web/src/components/cell-terminal-lifecycle.ts`) returns only for `lifecycleDisposed ||
runtime.unmounted`; it zeroes the cached cell box and calls `renderer.invalidateRowHeight()`
unconditionally, and gates ONLY `publishViewportNow()` on `shouldPublishActive()`. A background pane
therefore claims nothing while its font settles and measures the loaded face on its next claim. The
same callback is registered on the FontFaceSet's `loadingdone` and `loadingerror` (removed in
`dispose()`), because `ready` answers one loading epoch: a face that starts loading later settles
through those events alone, and a failed download still means re-measuring whatever face paints.
The renderer's own `fonts.ready` hook repairs history placeholders and bottom placement — it is a
different responsibility, not a substitute for invalidating the lifecycle's cell cache.

**Guard** — `apps/web/tests/cellTerminalLifecycle.fonts.test.ts` composes the real viewport
publisher with the real lifecycle: an inactive or pending pane publishes nothing while a wider face
settles and then claims the loaded advance, a later `loadingdone` / `loadingerror` reclaims an active
pane with no resize, and disposal claims nothing.
`smoke/terminal/terminal-mobile-font-width.spec.ts` proves it end to end — a real shell, a test-only
`size-adjust: 125%` face released while the SPA is hidden, then a DOM-Range check that the last
column's glyph is inside the mobile clip across both orientations.

---

## Terminal input, focus and keys

### Typing goes nowhere on a fresh mount

**Symptom** — "can't input anything in terminal on fresh mount / cursor blinks but typing goes nowhere / focusedClass=false even though textarea looks focused"

**Wrong** — rely on `.focus()` alone to fire focus events (it does not if the textarea was already
`activeElement` from a prior mount) / skip the mousedown click-recapture handler.

**Right** — the input textarea is off-screen, so clicks land on row spans, not the textarea; without an explicit
dance the focus listener never sees the event → the pane never reports focused → keystrokes go nowhere. The fix
lives in `apps/web/src/lib/terminalInputController.ts::forceFocus()`, and three pieces are load-bearing: (1)
`if (activeElement === textarea) textarea.blur()` BEFORE focusing — guarantees a fresh native focus event even
when the textarea was pre-focused; (2) an explicit `dispatchEvent(new FocusEvent("focus", { bubbles: true }))`
so pane styling is deterministic; (3) the container `mousedown` listener that calls `forceFocus` on every click.
Never leave the dance's re-focus guard latched on the error path or focus reporting dies for that pane's
lifetime.

**Guard** — `apps/web/tests/terminalInputController.test.ts`; `apps/web/tests/focusOwners.test.ts`.

### Borrowed receive-buffer view passed to a PTY write

**Symptom** — "backspace acts like space in terminal / paste burst drops chars / random byte substitution on PTY input"

**Wrong** — passing `f.payload` (a subarray VIEW onto the keeper's streaming receive buffer, per
`apps/worker/src/keeper/protocol.ts`) directly to `Bun.spawn`'s `proc.terminal.write(...)`. Bun's docs don't
promise synchronous consumption of the BufferSource argument, so the receive buffer can roll before the queued
write flushes. NOTE: the original "backspace = space" report was NOT this bug — it was `TERM=unknown` in the
spawned env (next entry). The defensive copy stays regardless: it is correct safety against the view-aliasing
class.

**Right** — **`Buffer.from(f.payload)` copy at the keeper PtyIn write site**
(`apps/worker/src/keeper/keeper-frame-handler.ts`). One copy per input frame, ~8 bytes typical, immeasurable on
the hot path. The same rule applies to ANY future `Bun.spawn` terminal write callsite that receives a borrowed
Buffer view.

**Guard** — `apps/worker/tests/keeper-input-stress.test.ts`.

### Bun.spawn does not inject TERM into the child env

**Symptom** — "backspace echoes wrong / Cmd-Backspace nukes prompt row / htop or vim crash with `ncurses: cannot initialize terminal type ($TERM=unknown)` — but ONLY on deployed workers, never on the local-bootstrapped one"

**Wrong** — assuming `Bun.spawn({terminal: {...}})` sets the child's `TERM`. It sets the PTY's internal `name`
but does NOT inject `TERM` into the spawned child's env; `node-pty` did this automatically, which is why moving
the keeper to Bun broke deployed workers but not the local one. The local worker inherited `TERM` from the
terminal that ran its original bootstrap; remote workers bootstrapped over non-TTY SSH inherited nothing → the
child shell sees `TERM=""`/`unknown` → zsh's ZLE cannot look up `cub1`/`el`/`ed` terminfo caps →
backward-delete-char emits just `0x20` instead of `0x08 0x20 0x08`, kill-line wipes the prompt row, and ncurses
TUIs refuse to start.

**Right** — **explicit `TERM: "xterm-256color"` in the env passed to `Bun.spawn`** at the keeper spawn site
(`apps/worker/src/keeper/keeper-frame-handler.ts`). Also set `LANG`/`LC_ALL` with `en_US.UTF-8` fallbacks so the
same SSH-bootstrapped env doesn't surface a locale bug next. Generalizable rule: any new
`Bun.spawn({terminal: {...}})` callsite MUST include `TERM` in env explicitly — Bun won't add it for you.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: keeper Bun.spawn env must set TERM explicitly (deployed-only ncurses $TERM=unknown)"`.

### An app shortcut swallows a control byte

**Symptom** — "Ctrl-F / a control key stops reaching the PTY after adding an app shortcut — `cat -v` shows the byte missing while the app UI opens instead"

**Wrong** — bind the chord anyway and try to `stopPropagation` selectively, or "fix" the test's expectation.

**Right** — **a capture-phase document handler on the pane runs BEFORE the key can be encoded, so it must never
claim a bare Ctrl+letter.** The terminal's own textarea handler is what `preventDefault`s a consumed control
byte, and every document-level BUBBLE listener already respects that — capture-phase bypasses it entirely.
Terminal-scoped chords use ⌘+key (macOS, never a PTY byte) or Ctrl+SHIFT+key (the gnome-terminal shape); find is
`⌘F / Ctrl+⇧F` for exactly this reason, resolved centrally in `apps/web/src/lib/browserPlatform.ts`. Before
adding one, check it is not a readline/TUI binding.

**Guard** — `smoke/terminal/` — `"terminal replay and Ctrl keys stay owned by the PTY"` asserts `^B^F^K`
round-trips.

### A global key router claims bare ↑/↓/⏎ on routes that have no cursor

**Symptom** — "I can't scroll" on a TV remote / D-pad — including on `/pair`; also "OK does nothing on
Request approval / Pair / Approve / Deny"

**Wrong** — a `window` CAPTURE-phase handler that `preventDefault()`s bare `ArrowUp`/`ArrowDown`/`Enter`
whenever no modal is open and no terminal deck is mounted, then routes them to a list cursor. Off the sidebar
that cursor's id list is EMPTY, so the keys move nothing while still cancelling the browser's work. Also wrong:
"fixing" it per-route, or gating on the route path — the predicate is whether a cursor target exists, not where
you are.

**Right** — **claim a key only when there is something to move.** `apps/web/src/lib/keyboardShortcuts.ts`'s
arrow/⏎ branch bails before `preventDefault()` on `!hasCursorTargets()` (arrows) and `cursorSessionId() === null`
(⏎), both from `apps/web/src/lib/sidebarCursor.ts`. Two distinct defaults are at stake and both are invisible
until they are gone: arrows are the DOCUMENT'S native scroll, and keydown `preventDefault()` cancels a focused
`<button>`'s click activation — so ⏎ on a real button dies silently with no console trace. Generalizable rule:
a capture-phase router must prove it will act before it cancels.

**Guard** — `apps/web/tests/keyboardShortcuts.test.ts` — `"↑/↓ stay the document's native scroll when no cursor
rows exist"` and `"⏎ stays a focused button's activation when no cursor row is highlighted"`;
`smoke/terminal/tv-dpad.spec.ts` asserts the `/pair` ArrowDown arrives with `defaultPrevented === false`.

### The first D-pad press does nothing because `<body>` counts as the origin

**Symptom** — a TV remote on a page that fits the screen (the unpaired pairing gate, any short route): ↓ never
reaches **Request approval** or any other control, focus stays on `<body>`, and no `dpad.nav` line is emitted.

**Wrong** — using `document.activeElement.getBoundingClientRect()` as the travel origin whenever it has size.
After load or a route change focus sits on `<body>`, whose box contains every control, so no candidate is ever
"beyond" it in any direction and the search returns nothing. Also wrong: autofocusing a button per page to paper
over it — every other route keeps the dead first press.

**Right** — `<body>` is never an origin. `apps/web/src/lib/spatialNavigation.ts` `pickTarget` treats
`active === document.body` as no origin and lands on the topmost-leftmost control, as its doc comment always
intended.

**Guard** — `smoke/terminal/tv-dpad.spec.ts` `"unpaired TV shows only the pairing gate and requests approval by
D-pad @tv"` presses ↓ from a fresh load until focus reaches `onboarding-pair-start-btn`, then ⏎ creates the
request with `defaultPrevented === false`.

### A terminal domain reset is treated as the input fence

**Symptom** — "Input may have been partially sent; it was not retried" after a terminal domain reset /
composer freezes ~10 s then reports ambiguous

**Wrong** — fencing terminal input on the terminal `domainGeneration`, so a `domain_overflow` /
`aggregate_overflow` / recovery reset on a LIVE socket settles every in-flight batch as ambiguous and then
discards the coordinator's late result; and returning silently from coord's terminal command gate when an
`input` command is refused, which leaves the browser waiting out `INPUT_RESULT_TIMEOUT_MS`.

**Right** — **the socket is the input fence.** Input results ride the CONTROL lane
(`apps/coord/src/connect/sync-ws-v2-control.ts` stamps `domain = UNSPECIFIED, domainGeneration = 0`), which no
domain reset touches, so a started batch keeps its 10 s deadline and settles from the real result;
`apps/web/src/ws/sync-outbound.ts::handleControl` correlates on `(socketId, sessionId, inputSeq)` plus the
generation coord echoes from the command, and `handleGeneration` only settles pendings when the SOCKET changed
(an unsent batch under the closing generation is `rejected`, never ambiguous). `resetSequence()` runs only on a
socket change, because a surviving pending must not share an `inputSeq` with a new batch. Coord's
`sync-ws-v2-commands.ts` answers every refused `input` with an `inputRejected` carrying the command's own
`domainGeneration` and the refusal reason — nothing reached a worker, so `rejected` is the truthful
classification and the composer restores the draft instead of claiming possible loss.

**Guard** — `apps/web/tests/syncOutbound.test.ts` — `"a terminal domain reset on a live socket keeps an
in-flight batch and settles it from the late result"`; `apps/coord/tests/sync-ws-v2-terminal-command-gate.test.ts`
— `"an input command for a resubscribing terminal domain is rejected, not dropped"`;
`apps/web/tests/terminalInputStatus.test.ts` — `"an unconfirmed batch with no written bytes never claims a
partial send"`.

### Delayed old-route input crosses a direct-promotion fence

**Symptom** — "a key sent on Sync appears after WebRTC became active / an old-route input reaches the PTY after
direct promotion".

**Wrong** — treat browser no-replay, a closed old socket, or a new renderer route as the input fence. A
coordinator→worker `DInputRequest` already in flight can arrive after the browser has promoted a direct route.

**Right** — the worker owns the fence: `TerminalInputRouteOwner` issues the actor/session route epoch, and
`writeTerminalInput` in `apps/worker/src/session-terminal-control.ts` rechecks live route authority after keeper
admission immediately before `beginInput`. A stale epoch returns `terminal input route changed`; it never writes
the PTY.

**Guard** — `apps/worker/tests/terminal-stream-input.test.ts` —
`"rechecks a live route after keeper admission before writing PTY input"`; real stack
`smoke/terminal/terminal-peer-failover.spec.ts` —
`"a delayed old Sync input is fenced after peer promotion and cannot reach the PTY"`.

### node-datachannel `sendMessageBinary(false)` is accepted buffered delivery

**Symptom** — "a direct terminal fragment duplicates after WebRTC backpressure / a false native send result
resends a control, cell, or history fragment".

**Wrong** — interpret `node-datachannel` `sendMessageBinary(...) === false` as refusal and retry the fragment.
The native channel accepted it into its buffer, so retry duplicates protocol bytes.

**Right** — `TerminalPeerPacketPort` commits the fragment exactly once; `false` marks the lane
`backpressured` and waits for its low-water callback. Queue refusal happens before the native call; a native
throw retires the peer.

**Guard** — `apps/worker/tests/terminal-peer-packet-port.test.ts` —
`"commits a native false return once without retrying its accepted fragment"`.

---

## Worker, keeper and host

### Pane close races the worker reading the kill

**Symptom** — "pane ✕ click does nothing"

**Wrong** — send kill + immediately `conn.close()` (the browser close frame races the worker reading kill).

**Right** — the worker's kill path synchronously acks with a `closed` control message
(`apps/worker/src/session-lifecycle.ts`); the browser waits for that ack before tearing down.

**Guard** — `smoke/terminal/` — `"browser smoke flow creates and cleans its resources"` (drives pane close end
to end).

### A worker throttled by its own cgroup looks healthy

**Symptom** — "a worker shows offline/down in the SPA while `systemctl --user status roost-worker` says active (running) and the host has GBs free / worker log silent for minutes then `link_stale_no_downstream` + `listChannels timed out` + `heartbeat beat failed [unavailable] HTTP 502` / coord `worker-ws close`→`open` gap of ~361s"

**Wrong** — chase the 502 into the front-door proxy, restart the worker, or read the SPA's host metrics and conclude
the box is healthy — `apps/worker/src/host-sample-linux.ts` reads host-wide `/proc/meminfo`, so a unit strangled
by its own `MemoryHigh` publishes "8.7 GB of 33.6 GB used" while every allocation in its cgroup is throttled;
equally wrong: adding `MemoryMax` (every PTY session shares this cgroup, so a hard cap plus `Restart=always`
turns one fat session into a fleet-wide session wipe). Measured on a live host: cgroup
`memory.current=3401814016` vs `memory.high=3221225472`, `memory.events high` climbing ~150k/min, worker MainPID
in `D (disk sleep)`, 6 PTY sessions = 2.9 GB in the SAME cgroup, `SwapFree 172 kB` so reclaim had nowhere to go.

**Right** — **three layers, all required.** (1) `MemoryHigh` must scale with the host:
`apps/worker/scripts/install.sh::default_worker_mem_high` is 60% of MemTotal, floor 3G, absolute (systemd only
takes % from v240); `TasksMax=4096`, not 512. The live value can sit in a hand-written
`~/.config/systemd/user/roost-worker.service.d/limits.conf` drop-in that OUTRANKS the deployed unit body — check
the drop-in before editing the unit. (2) A dial that never fires `ws.onopen` is NOT an auth rejection: coord
answers a bad JWT with an HTTP 401 upgrade, indistinguishable from a timeout or a proxy 502 in Bun's client
`WebSocket`, so throttle-induced dials used to arm the auth-reject backoff cap and turn a ~20s stall into ~6 min
of "down". `apps/worker/src/transport/coord-link-constants.ts::backoffCapMs(streak, hasOpened)` keys escalation
on `hasOpened`; the log is `reconnect_backoff_escalated`, never `auth_rejection_escalated`. (3)
`sampleCgroupPressure` + `apps/worker/src/heartbeat.ts::logCgroupPressure` emit
`cgroup_memory_high_exceeded`/`_cleared` so the next occurrence is one grep, not a guess.

**Guard** — `apps/worker/tests/coord-link-backoff-cap.test.ts`.

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

**Guard** — `apps/coord/tests/worker-session-list-auth.test.ts`;
`apps/worker/tests/boot-reconcile-admission.test.ts`.

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
`wtermCore.resize(cols, rows)` on the existing core, resets only the cell
emission baseline/epoch, and emits one viewport-only full. Primary and
alternate grids, modes, links and representable scrollback stay in memory.
Keeper-history replay is reserved for genuine worker adoption when no live core
exists; an unprovable resize boundary fails closed.

**Guard** — `apps/shared/tests/wterm-resize-in-place.test.ts`;
`apps/worker/tests/terminal-stream-state.test.ts`;
`apps/coord/tests/terminal-view-registry-membership.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`.

### Quoting a systemd path directive because quoting is "safer"

**Symptom** — "`roost push` stages the release and then fails activation: `Unit roost-coord.service has a bad unit file setting` / `WorkingDirectory="/home/user/roost": path is not absolute`, the push rolls back, and the whole fleet stays pinned at the older commit while every Linux coordinator/worker deploy fails identically / or the unit starts clean and writes NO logs — `main.out.log` never grows and the journal carries `Failed to parse output specifier`"

**Wrong** — treat systemd quoting as universal and pipe every dynamic value through
`systemd_quote()` in `apps/coord/scripts/install.sh` / `apps/worker/scripts/install.sh`. It is tempting because
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
the output specifier fails to parse and is dropped with no error and no log file. `systemd_path()` sits next to
`systemd_quote()` in both installers: it rejects a value containing newline, CR or `"` (the characters that
would let a value forge a directive line, which is the only thing the quoting bought), doubles `%` so the value
can never be read as a systemd specifier, and emits it raw. `WorkingDirectory=`/`StandardOutput=`/`StandardError=`
use `systemd_path`; `ExecStart=` keeps `systemd_quote`, and `systemd_env` keeps its own quoted
`Environment="KEY=VALUE"` form (which is legal there). Second-order lesson:
writing a unit file is not activating a service — activation must be proven by systemd actually STARTING the
unit, which is exactly what caught this. `apps/roost-cli/src/push.ts` never saw a healthy coordinator at the
expected SHA, took its `rollback-prior` branch and restored the previous release, so the fleet sat on an old
commit instead of "succeeding" onto a dead one; a deploy path that trusted "the unit file was written" would
have reported success against a coordinator that was never running.

**Guard** — `apps/roost-cli/tests/systemd-unit-quoting.test.ts` — generates both units through each installer's
`write-plist` verb and runs `systemd-analyze --user verify` on them, so a re-quoted path directive fails in CI
rather than on the first `roost push`.

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

**Guard** — `apps/roost-cli/tests/install-plist-write.test.ts` removes the fake
`Library/LaunchAgents` directory and proves both installers recreate it before
publishing a mode-0600 plist.

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
live in one static table in `apps/roost-cli/src/deploy-plist-env.ts` (`DEPLOY_IDENTITY_ENV_FLAGS`, which also
names the `roost deploy` flag that supplies each), and `_resolveDeployEnvValue` takes an explicit
`target: "self" | "remote"` saying whose machine the ambient env describes. For `"remote"` an identity key
resolves only from the invocation flag (`--label`, `--reachable-addr`) or the target's own installed plist /
unit; for `"self"` the ambient env is the target's own and stays valid. Unresolvable is not an error by itself —
the worker derives its hostname and tailnet name, which is the documented fresh-target path — but
`resolveRemoteDeployIdentityEnv` REFUSES the deploy (`failDeploy(6, …)`) when the deploying shell exports that
key and nothing else resolved it, because that is exactly the ambiguity that mislabels a fleet. Fleet-wide keys
(`ROOST_COORDINATOR_URL`, `ROOST_BOOTSTRAP_TOKEN`, diag flags) keep the ambient fallback.

**Guard** — `apps/roost-cli/tests/deploy-identity-env.test.ts` — a fresh remote target with an ambient
`ROOST_WORKER_LABEL` resolves to nothing and refuses with the flag named, while the flag value, the target's
installed value, and a `self` deploy each still resolve.

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

**Guard** — `apps/worker/tests/agent-status-integration-ownership.test.ts` — an asset marked on line 106 is
adopted, overwritten byte-for-byte and accepted by the commit guard, while a code-line mention, a near-miss
token and an unmarked file stay refused; `apps/worker/tests/agent-status-installer.test.ts` pins that a
user-owned pi target and a symlinked omp target each fail alone while every other asset installs.

### A one-shot deploy flag stops at the installer process

**Symptom** — "`roost deploy <host> --force-live` printed the destructive-authorization banner, staged, wrote
the plist — and the worker then EXITED with `keeper_survivor_identity_unproven` / `keeper endpoint is held by a
process that did not prove keeper identity; stop that process, then restart the worker`", so the operator has
to stop the service, kill the legacy keeper and delete the mux socket by hand — which is the exact work the
flag exists to avoid.

**Wrong** — treat "the flag is in the composed install environment" as "the flag reached the worker". A POSIX
deploy runs `<composed env> bash apps/worker/scripts/install.sh write-plist` over ssh, so every variable in
that prefix is real — in the INSTALLER's process. `install.sh` then writes an explicit key set into
`EnvironmentVariables` / `Environment=`, and a key absent from that set dies with the installer's shell: the
worker launchd/systemd starts never sees it. Nothing warns, because both ends are individually correct — the
CLI composed the value (`deploy.ts`, `deploy-macos.ts`, `deploy-local.ts` all pass
`ROOST_KEEPER_FORCE_LIVE_RETIRE`), `config.ts` parses it, `boot-keeper.ts` branches on it, and the deploy log
line `>> reused from existing plist on <host>: …` even proves the environment merge worked. The worker simply
booted on the non-force path and refused, which reads as "the flag was ignored" rather than "the flag was
never installed".

**Right** — **a value only reaches the service if the service DEFINITION carries it.** `install.sh` emits
`ROOST_KEEPER_FORCE_LIVE_RETIRE` beside `ROOST_BOOTSTRAP_TOKEN` in both `write_plist` and `write_unit`, and
never reads it back off an installed definition (unlike `ROOST_AGENT_CONVERSATION_RESTORE`, whose installed
choice is deliberately preserved) — an invocation not given the flag simply omits the key. Writing a
destructive authorization into a definition then creates the opposite hazard, a flag that re-authorizes
discarding live PTYs on every later restart, so it is one-shot on BOTH sides: the activation that reads it
spends it (`spendKeeperForceLiveRetireAuthorization` in `apps/worker/src/service-definition-env.ts`, the same
keyed erasure the redeemed bootstrap token uses, called from `main.ts` before any keeper work), and the next
deploy strips an installed value anyway (`workerInstallEnvironmentValues`). The force branch also names what it
destroys BEFORE requesting the shutdown — `keeper_binding_channel_ids` / `spawning_channels`, `null` when the
survivor could not enumerate them, which is why the authorization was needed at all.

**Guard** — `apps/roost-cli/tests/deploy-keeper-force-live-authorization.test.ts` — drives the real
`install.sh write-plist` with the composed environment and pins that the definition carries the flag alongside
values reused from a prior install, that `loadWorkerConfig` then reads it as armed, and that after the boot
spends it neither the definition, the following deploy's environment, nor the reinstalled definition carries it;
`apps/worker/tests/keeper-legacy-retire.test.ts` — an authorized boot logs the discarded bindings before the
retirement, spends its own authorization, and the same survivor still yields `KEEPER_IDENTITY_UNPROVEN` without
the flag.

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
stands.** `installedServiceRefusalAfterTargetEvidence`
(`apps/roost-cli/src/keeper-admission-staging.ts`) runs ONE probe on the host and stages only on positive proof
of emptiness: the service definition is absent, or the service manager itself answered AND reports the worker
not running AND no keeper process is parenting a channel process. Every unknown fails closed, because an
unreachable service manager reads exactly like a stopped one in its own output: darwin corroborates a failing
`launchctl print` with a `launchctl print-disabled` domain query (an unloaded job and an unreachable launchd
share an exit code), Linux requires `systemctl show` — which exits 0 even for a unit it has never heard of — to
exit 0, and a host whose PATH has no `pgrep` cannot prove no keeper is alive and is refused. A keeper SOCKET
FILE is deliberately not evidence: it outlives the keeper that created it, so it can neither prove nor disprove
anything the process counts do not. Live PTYs keep every refusal they had — a running worker, or a keeper
holding channels, still refuses — and a permitted install prints one line naming the evidence that allowed it.

**Guard** — `apps/roost-cli/tests/keeper-admission-staging.test.ts` — runs the generated probe through a real
shell against stub `launchctl`/`systemctl`/`pgrep` binaries: a stale row over a target running nothing stages,
the same row over a running worker or over a keeper holding channels refuses, a prior macOS service that is
merely not loaded stages, and an unreachable service manager or a missing `pgrep` refuses instead of reading as
empty.

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

**Guard** — `apps/roost-cli/tests/deploy-roll-forward-recovery.test.ts`: "a retained macOS rollback whose
prior release cannot run rolls forward", "only a started prior release with a migrated store is
unrecoverable", "an ordinary prior-proof failure still keeps the macOS journal", "a schema-4 Linux journal
parses and still rolls back", plus the two probe/checkpoint tests that pin the recorded version.

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

**Guard** — `apps/roost-cli/tests/deploy-local-release-retirement.test.ts`: "an rsync-staged prior release
is retired even though it is no git worktree", plus the confinement and no-prior cases.

### A retired release's dist leaves every page a 404 while the API still answers

**Symptom** — "every page is 404 / `not found` in the browser but the API and terminals still work / the UI
died after a deploy or a reboot".

**Wrong** — trust the `ROOST_WEB_DIST_PATH` a deploy stamped into the installed service. It points INTO a
release directory that a later settlement deletes
(`apps/roost-cli/src/deploy-plist-env.ts:118-122` records why the value is never carried forward), and
`createSpaResponder` then picks no source at all: `apps/coord/src/coord-factory.ts`'s SPA arm answers the
bare `not found` 404 for `/` and every deep link, which reads like an edge, DNS or certificate fault
because the RPC surface on the same listener is untouched. Chasing the front door here costs the outage.

**Right** — the SPA source is startup-visible state, not something to infer from a 404.
`createSpaResponder` reports the build it chose (`source: "disk" | "embedded" | "none"`, derived from that one
choice and never re-probed), `apps/coord/src/main.ts` logs `spa_source_missing` once when that is `"none"` and
keeps serving — worker links and keeper state outlive a browser build that went away. `roost status` must not
repeat the inference: the CLI cannot read a released install's embedded manifest, so it HEADs the
coordinator's own root and reports that answer next to the stamped path. A source install points
`ROOST_WEB_DIST_PATH` at `$REPO_ROOT/apps/web/dist`, which no settlement deletes; a released install
re-stamps it per deploy.
These three make the state visible; what stops the commonest way INTO it is the next entry, "An installer
inherits a sibling service's dist path from the shell that ran it".

**Guard** — `apps/coord/tests/spa-source-startup.test.ts` "a retired web dist is reported once at startup,
not only as a page 404", `apps/shared/tests/spa.test.ts` "names the build it serves, so an empty pick is
reportable instead of a bare 404", and `apps/roost-cli/tests/status-spa.test.ts`, including "a compiled
install serving its embedded build is not called missing".

### An installer inherits a sibling service's dist path from the shell that ran it

**Symptom** — a coordinator unit whose `ROOST_WEB_DIST_PATH` points inside
`RoostWorkerV2/service/releases/worker/…` (or a worker unit pointing into the coordinator's releases), so the
UI dies the next time the OTHER service deploys and retires that release.

**Wrong** — read `ROOST_WEB_DIST_PATH` straight out of the environment in `write_plist`/`write_unit`:
`web_dist="${ROOST_WEB_DIST_PATH:-$REPO_ROOT/apps/web/dist}"`. The variable arrives from whatever ran the
installer, and the programmatic callers pass the ambient environment through —
`runInherit` in `apps/roost-cli/src/quickstart-runtime.ts:28-37` spawns with `{ ...process.env, ...env }`, and
the env it merges (`coordinatorEnvironmentForQuickstart`) names a bind, a public URL and
`ROOST_SKIP_ENV_LOCAL`, nothing about the dist. So a dist exported for a DIFFERENT service silently lands in
this service's definition. The CLI's carry-forward already strips the key
(`apps/roost-cli/src/deploy-worker-environment.ts:26-39`) — the hole was the shell installers, which are also
what `roost status` and GETTING_STARTED tell an operator to run by hand. Do not fix this by scrubbing the key
at each caller: the installers are the single owner of what their own unit may name.

**Right** — `resolve_web_dist()` in both installers honors an explicit value only when it resolves inside that
install's own root, and otherwise warns on stderr and stamps `$REPO_ROOT/apps/web/dist`. Every legitimate
caller satisfies it: `push-coordinator.ts:279-291` sets `ROOST_REPO_ROOT` to the release it staged, and the
worker's remote activation runs `<release>/apps/worker/scripts/install.sh` with that release's own dist, so
the script-derived root already contains it. The worker installer takes no `ROOST_REPO_ROOT` override at all
(`apps/worker/scripts/install.sh:7`), which is why its root cannot be spoofed. The check never fails an
install — a compiled install serves its embedded build regardless.

**Guard** — `apps/roost-cli/tests/coord-installer.test.ts` "a dist path from another service's release tree is
refused, not stamped" (runs BOTH installers, on the Linux and Darwin writers) and "a dist inside this
install's own root is stamped as given".

---

## Transport and connection lifecycle

### Connect/gRPC bidi under Bun for the worker↔coord stream

**Symptom** — "sessionsSpawn → [internal] internal error / spawn hangs forever / worker↔coord bidi flaps every ~10-30s / connect-node 'h2 is not supported' tight-loop"

**Wrong** — Connect-bidi (`WorkerService.Attach` via connect-node) for the worker↔coord stream UNDER BUN: h2
throws "[internal] h2 is not supported" (Bun's `node:http2` is incomplete) → tight reconnect loop; over h1.1
`Bun.serve` buffers the long-lived request body so the worker's upstream replies never reach coord → every spawn
hangs; AND `Bun.serve`'s default `maxRequestBodySize` (128 MB) caps the long-lived h1.1 attach body (TUI redraws
fill it in ~10-30s) → flap. Re-registering the bidi service or flipping the link's `httpVersion` to "2"
reintroduces all of it.

**Right** — **raw Bun WebSocket** at `/ws/coord-worker/:fp?token=<jwt>` carrying the SAME proto frames as binary
(`toBinary`/`fromBinary`) — coord `apps/coord/src/connect/worker-ws-handler.ts` (sharing `makeWorkerConn` + the
`connectWorkers` registry), worker `apps/worker/src/transport/coord-link.ts::dial()`. Auth is a query-param JWT
(Bun's CLIENT `WebSocket` has no custom-header API). NEVER run a Connect/gRPC bidi through Bun.

**Guard** — `apps/coord/tests/worker-ws-transport.test.ts`; `scripts/lint-roost.ts` rule `"phase-24: `new
WebSocket(` outside the canonical client/server links"`.

### Half-open WS survives a coord restart and never closes

**Symptom** — "new terminal → [failed_precondition] worker … not connected / worker log silent (no stream_error) for hours / heartbeats fine, lsof shows ESTABLISHED to :4102"

**Wrong** — restart the worker by hand / trust `ws.onclose`. When the coord process dies and is relaunched,
a TLS-terminating front door keeps the worker-side TCP ESTABLISHED, so `ws.onerror`/`ws.onclose` NEVER fire and `ws.send`
(including in-band JWT refresh) black-holes forever; the restarted coord's in-memory `connectWorkers` registry
has no WS for the fingerprint → the hub socket lookup returns null →
`apps/coord/src/connect/handler-session-spawn.ts` throws failed_precondition on every spawn while heartbeats (a
separate unary transport) keep the row looking alive.

**Right** — **a stale-link watchdog on the worker side** in `apps/worker/src/transport/coord-link.ts`
(`dial()`'s open/message handlers): coord pings every 30s (`apps/coord/src/connect/worker-conn.ts`); every
downstream frame stamps `lastDownstreamAtMs`; a per-dial interval (`STALE_CHECK_INTERVAL_MS` 15s) force-closes
and re-dials after `STALE_LINK_TIMEOUT_MS` 90s (3 missed pings) of downstream silence → hello→snapshot replay
heals the rest. Same half-open-behind-a-proxy class as the boot RPC timeout.

**Guard** — `apps/worker/tests/coord-link-stale-watchdog.test.ts`.

### Cold start loses the event published between snapshot and socket

**Symptom** — "brand-new browser: spawning a terminal does nothing — no pane, no sidebar row, store `sessions` stays empty until a reload / 'works on the second load'"

**Wrong** — dial the Sync socket after the bootstrap lists again (throws away the cold-start win for every warm
boot to fix only the first-ever boot), or paper over it with a post-bootstrap `sessionsList` refetch.

**Right** — **the snapshot must be ordered AFTER the socket is subscribed.**
Coordinator Sync runs no backfill from zero, so an event published between
`sessionsList` resolving and the socket subscribing is lost outright — there is
nothing to replay it from. `apps/web/src/store/sync-bootstrap.ts` awaits the
subscribed barrier (`waitForSyncSubscribed` in `apps/web/src/store/sync.ts`,
which resolves only once the subscription establishes socket/domain
generations, never at `WebSocket.onopen`) and takes its snapshot against that
socket's id, so the window is CLOSED, not merely shrunk. Browser authorization
finishes separately through one-shot grant redemption or pairing before this
pipeline proceeds; an unknown key remains in onboarding and is not silently
enrolled or retried as a transport failure.

**Guard** — `smoke/terminal/` — `"browser smoke flow creates and cleans its resources"` on a FRESH context;
`apps/web/tests/sync-flow.test.ts` — `"repeated bootstrap retries retain one infinite-loop owner"`.

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

**Guard** — `apps/coord/tests/device-revocation.test.ts` (a tailnet address
without a grant remains unauthorized) and `apps/coord/tests/pair-bus-publish.test.ts`
(a tailnet caller cannot self-approve).

### audit_log caller_fp is NULL for every authed RPC

**Symptom** — "audit_log shows caller_fp=NULL for every authed Connect RPC"

**Wrong** — writeAuditLog from the outer fetch wrapper in `coord-factory.ts` — the auth interceptor sets
caller_fp on per-RPC contextValues which the outer wrapper can't see; bridging via AsyncLocalStorage works but
the indirection rots on the next async-layer addition.

**Right** — **writeAuditLog INSIDE the AuthInterceptor's try/finally** at
`apps/coord/src/connect/auth-interceptor.ts`. The interceptor has the caller (just verified), the path
(`/${service}/${method}`), the trace id (header) and the status (200 on success; the mapped HTTP status on a
ConnectError throw). `coord-factory.ts` only audits non-Connect paths (db-export, SPA, 404) where a null caller
is structurally correct.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: writeAuditLog must be CALLED inside the AuthInterceptor (else audit_log caller_fp=NULL)"`.

### A mutation commits without publishing its bus delta

**Symptom** — "task state changes invisible to other browsers — Browser A claims/done, Browser B's QueueView keeps showing prior state until refresh"

**Wrong** — enqueue publishes `created`; the next-pending / set-state / cancel handlers do their DB UPDATE but
never publish, and sync-stream backfill by event id doesn't recover it because in-memory bus deltas aren't in
the events table.

**Right** — **`publishTaskState(row)` at every UPDATE-returning point** in
`apps/coord/src/connect/handlers-tasks.ts`. Every mutation handler whose domain has a `*Bus` MUST follow
`db.updateTable(...).executeTakeFirst/Throw()` with the matching `publish*State(row)` in its own
`connect/handlers-<domain>.ts`. Bus message shapes live in `apps/coord/src/buses.ts`.

**Guard** — `apps/coord/tests/task-bus-publish.test.ts`.

### Rate-limit buckets matched by path prefix

**Symptom** — "rate-limit prefix matches read-only list calls — bootstrap traffic + tab focus refresh burn the same bucket as mutations, 429-cascade on legitimate writes"

**Wrong** — path-prefix match (one prefix catches both List and the mutations) plus an
`if (req.method === 'GET') return null` bypass — but Connect-ES emits every unary RPC as POST, so the bypass
never triggers.

**Right** — **`RATE_LIMITED_ROUTES: ReadonlySet<string>` enumerating mutation paths only** at
`apps/coord/src/middleware/rate-limit.ts`: auth (MintBootstrap/RedeemWorker/RedeemBrowser),
workspace create/update/delete/set-sessions, task enqueue/set-state/cancel, MCP mutations, and worker
rename/delete/deploy-start. `*List`, identity and health probes are NOT in the set.

**Guard** — `apps/coord/tests/coord-e2e.test.ts` —
`"rate limit: 100 AuthRedeemBrowser POSTs from same IP → 101st returns 429"`.

### JSON.parse inside a bus publish, after the commit

**Symptom** — "RPC returns 500 but DB row IS persisted, SPA UI keeps showing prior state until manual refresh"

**Wrong** — raw `JSON.parse(row.X)` inside a `bus.publish({...})` payload construction AFTER the surrounding
mutation committed — a partial-write or hand-edited row throws SyntaxError, the RPC 500s, the bus subscriber
never fires, and sync-stream backfill does not recover in-memory bus deltas.

**Right** — **`safeJsonParse` from `@roost/shared/json`** with a fallback matching the consumer schema (`{}` for
non-nullable record fields, `null` for nullable ones). Request-time validation (reject upfront with a
ConnectError) is the OTHER pattern — it applies BEFORE the DB write, not after.

**Guard** — `scripts/lint-roost.ts` rule
`"L11: raw JSON.parse() inside a *Bus.publish() payload — parse-after-commit 500s the RPC → split-brain; use safeJsonParse"`.

### A coordinator-global setting stored in a dashboard scope bricks self-hosted boot

**Symptom** — "fatal: self-hosted tenant invariant violation: app_settings contains invalid dashboard
scope" — the coordinator exits at startup on a database whose `push.vapid` keypair carries a
`dashboard_id`.

**Wrong** — relax the guard, or hand-delete the offending row on the live database. Also wrong: the
drift that causes it — writing `push.vapid` with a `dashboard_id`, when `apps/coord/src/vapid.ts`
reads and writes that keypair only at the explicit NULL scope, so a scoped copy is unreachable by
every code path that exists.

**Right** — the guard is correct (`apps/coord/src/self-hosted-tenant.ts` owns it), so repair the data in a
numbered migration: `apps/coord/migrations/0029_global_push_vapid_identity.sql` drops the unreachable
scoped copies, and promotes the newest one to NULL scope when no global row exists rather than
discarding the identity that signed the live push subscriptions. A coordinator-global setting belongs
in the NULL scope; every dashboard-scoped key stays scoped.

**Guard** — `apps/coord/tests/push-vapid-scope-migration.test.ts`: the live shape (a global row plus a
scoped duplicate) fails admission before the migration and admits after it, promotion keeps the sole
scoped identity, and a NULL-scoped ordinary key is still refused by the tenancy guard.

---

## Browser platform reality

### A defaulted injectable host function loses its receiver

**Symptom** — "browser-only feature silently dead while its unit tests pass / `Illegal invocation` swallowed inside a bus subscriber"

**Wrong** — keep injectable-timer fields as bare `setTimeout`/`clearTimeout`, and trust unit tests that run in
Bun.

**Right** — **wrap host functions when defaulting an injectable:
`options.setTimer ?? ((cb, ms) => setTimeout(cb, ms))`** (`apps/web/src/lib/agentNotificationCore.ts`). Stored
bare, `this.setTimer(...)` calls `window.setTimeout` with the instance as receiver → `Illegal invocation` in a
browser, harmless in Bun. Publish loops catch subscriber throws, so the only visible symptom is "nothing
happens" — a live browser pass is what catches it.

**Guard** — `none` — Bun unit tests pass either way; only the live/Playwright browser pass exercises the
receiver.

### lib.dom types are the spec surface, not the engine's

**Symptom** — "a DOM option silently does nothing in the browser while `tsgo` is green / `<input capture>` opens the file browser instead of the camera / an assignment to a documented DOM property never reaches the attribute"

**Wrong** — trust the type checker: lib.dom declares the property, so the assignment typechecks and reads as
done. Equally wrong once it misbehaves: widen the type, cast to `any`, or relax an unrelated header
(`permissions-policy: camera=()` does NOT gate `<input capture>`) — the checker was never the problem.

**Right** — **for any HTML attribute whose IDL reflection is not universal, set the ATTRIBUTE
(`input.setAttribute("capture", …)`, `apps/web/src/lib/attachments.ts`) and assert `getAttribute` in a test.** A
green typecheck is not evidence that a DOM property exists at runtime, and a browser-only no-op has no stack
trace, so unit tests that never touch a real engine stay green. The tripwire must assert the ABSENCE first
(`expect("capture" in makeInput()).toBe(false)`) or a fake DOM that later grows the field silently retires it.
Measured in the live tab: `"capture" in document.createElement("input")` → **false** on Chromium 150, so the
property assignment became an expando and `getAttribute("capture")` stayed `null`.

**Guard** — `apps/web/tests/attachmentsPicker.dom.test.ts`.

### An unbounded await in the device-open path parks forever

**Symptom** — "mobile mic records once then never again / stop leaves the UI animating / phone recording indicator stays lit until reload"

**Wrong** — treat it as a network problem (the socket-open timing says the socket was fine), or as the
silent-mic class — the silence watchdog is armed FROM capture's resolution, so a start that never resolves has
nothing watching it; equally wrong: lengthen the mobile idle window so tap #2 reuses a warm pipeline, which only
hides the cold re-open that re-rolls the WebKit dice.

**Right** — **every await in the device-open path is bounded and a failed open disposes what it built.**
`micTimeouts` (open/resume/module) in `apps/web/src/lib/audioPcmCapture.ts` wraps `getUserMedia`,
`AudioContext.resume()` and `audioWorklet.addModule()` — WebKit returns promises that NEVER settle while the OS
audio session is mid-transition, and an unbounded await left the warming slot non-null for the page's lifetime
(every LATER tap awaited the same dead promise) and the starting-captures count above zero forever (so
`releaseMicIfIdle` never released the device). `openPipeline` builds into LOCALS and publishes the singleton in
one step, so a stalled open that settles late cannot clobber the pipeline a later tap already built. Every async
continuation in a recording carries a run token (`apps/web/src/lib/deepgramDictation.ts`, bumped in teardown),
because completing a send resets the end-intent to null and null ALSO means "a recording is live" — that is how
a stopped recording's grant opened a socket onto the shared connection and killed the NEXT recording. Finalizing
has a watchdog and stays tappable.

**Guard** — `apps/web/tests/deepgramDictation.test.ts`; `apps/web/tests/audioPcmCapture.test.ts`;
`smoke/terminal/` — `"a second recording works exactly like the first"`.

### A diagnostic sink throws into the path it was observing

**Symptom** — "terminal input silently dies once SPA diagnostics are on" /
`TypeError: Do not know how to serialize a BigInt` / `JSON.stringify cannot serialize cyclic structures`,
thrown from a `diag()` call inside the send path.

**Wrong** — `JSON.stringify(kv)` raw in a diagnostic sink, then repairing the one call site that blew up
(`input_seq: String(pending.inputSeq)`). A proto `uint64` is a `bigint`, so the sink threw out of
`diag("bytes.up_send", …)` back into `sendTerminalInput` and killed every keystroke; per-call-site
stringification leaves every future call site armed with the same trap.

**Right** — **observability can never propagate a failure into a product path.** `safeJsonStringify` in
`apps/shared/src/json.ts` (the repo's canonical JSON boundary, which already owned the parse direction) maps
`bigint` to its exact decimal string at every depth — `Number()` is forbidden, it rounds past 2^53 — and
returns the caller's fallback instead of throwing; `apps/web/src/lib/diag.ts` ships
`{"kv_unserializable":true}` so the event still reaches the operator with the loss flagged. `emitEnabled()`
and `signal()` in `apps/shared/src/diag.ts` wrap record construction (the `...kv` spread runs getters) AND
sink dispatch in one guard per function that reports through `log.warn` with strings only, so a hostile value
cannot re-throw on the reporting line. One guard at the facade covers every sink.

**Guard** — `apps/web/tests/diag.test.ts`; `apps/shared/tests/json.test.ts`.

### env(safe-area-inset-*) is 0px on a television, and a portal escapes the shell's padding

**Symptom** — "buttons/keys are cut off at the edge of the TV screen / I can't see the bottom-right control on
the TV"

**Wrong** — rely on `env(safe-area-inset-*)` to keep chrome off a bezel-cropped edge. TV browsers report all
four as `0px`, so the padding that protects an iPhone notch protects nothing here. Equally wrong: add the
overscan gutter only to `.workbench-shell` — a `position: fixed` surface portaled to `<body>`
(`TerminalNavButtons`, `apps/web/src/components/TerminalNavButtons.tsx`) is not inside that box and keeps its
own viewport-relative offsets.

**Right** — **explicit overscan tokens, applied to the shell AND to every portaled fixed surface.**
`--tv-overscan-inline` / `--tv-overscan-block` are declared in `apps/web/src/styles/theme-vars.css` (~2.5% of a
1080p frame) and applied under `[data-tv="true"]` in `apps/web/src/styles/tv.css`. When raising a fixed
surface's `bottom`, raise any `max-height` that subtracts a literal mirroring that offset — `.term-nav`
subtracts a `220px` twin of its own `bottom`, so a raised offset without a matching subtraction lets the sheet
run off the TOP of the frame. Subtract the block overscan twice: once for the raised bottom, once to keep the
surface's own top edge clear.

**Guard** — `smoke/terminal/tv-dpad.spec.ts` reads the tokens off the computed root and asserts both portaled
surfaces sit inside the safe rect on the right, bottom AND top edges; removing the `tv.css` override fails it at
`16px` against the `48px` inline overscan.

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
`RETIRED_MIGRATIONS` (`apps/coord/src/db/migrate.ts`) and compare the surviving rows only, never by
position in the raw history — a reused slot number means the retired name can sort before the
migration that replaced it. An unknown name must still fail closed.

**Guard** — `apps/coord/tests/retired-migration-history.test.ts`: one case drives `runMigrations`
over a database carrying the retired name plus the migration that reused its slot and requires the
remaining chain to apply; the sibling case requires an unrecognized row to still throw.

### Redesigns discard previous fixes

**Symptom** — "sidebar redesign loses every previous fix"

**Wrong** — "phase-N: complete sidebar rewrite".

**Right** — additive commits behind a flag; the smoke flow must still pass after each.

**Guard** — `smoke/terminal/terminal-delivery.spec.ts` `"browser smoke flow creates and cleans its
resources"` re-runs the whole flow on every CI run (`runFlow`: workspace create → terminal open → PTY
marker round-trip → pane close → cascade-delete), plus the deck-persistence cases in
`smoke/terminal/terminal-render-deck.spec.ts`. Gap: nothing asserts that a *named* earlier fix
survived a rewrite — only that the flow, the deck, and the `scripts/lint-roost.ts` sidebar rules hold.

### An anonymous 401 from the internet writes a row nothing ages out

**Symptom** — `audit_log` grows without bound on a coordinator behind a front door, filled with
`status=401` rows whose `caller_fp` is NULL; the sweep runs and deletes none of them.

**Wrong** — delete `shouldPersistConnectAudit` (`apps/coord/src/middleware/security.ts`) because its
body reduces to a constant once the listener it named is gone, or answer the growth by widening
`AUDIT_SWEEP_METHODS`. Both read as simplification and both re-open the hole: the sweep in
`apps/coord/src/audit-retention.ts` is an explicit allowlist (`SessionsInput` only) that must never
age out auth rows, so an unauthenticated scanner's row is permanent.

**Right** — keep the predicate and skip exactly the anonymous 401 that arrived through a trusted
proxy. It carries no identity — `audit_log` has no address column — so it is unbounded volume with
no forensic value, while a 401 that names a device, any other status, and every request on a
`direct` listener still persist. Telemetry counters and cooldown-coalesced signals cover the
anomaly the rows would have shown.

**Guard** — `apps/coord/tests/audit-policy.test.ts` `"skips only an anonymous 401 that arrived
through a trusted proxy"` pins all four boundaries: anonymous 401 + `trusted-proxy` skipped;
the same 401 with a device fingerprint persisted; an anonymous 403 persisted; an anonymous 401 on a
`direct` listener persisted.

---

### An incident bundle reports a layer "unavailable" that actually sent its evidence

**Symptom** — `bun scripts/replay-terminal-incident.ts <bundle>` prints `section browser:
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

**Guard** — `apps/shared/tests/terminal-capture-envelope.test.ts` pins both halves: a flattened
payload is refused at the envelope naming the missing layer member, and an envelope placed where a
section belongs fails `validateTerminalIncidentBundle` at `browser.captured_at_ms`. Producer-side,
`apps/worker/tests/terminal-capture-evidence.test.ts` and
`apps/coord/tests/terminal-capture-recorder.test.ts` assert a real capture lands non-null
`bundle.browser` and `bundle.coordinator` sections with zero `remote:` omissions.

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

**Guard** — `smoke/terminal/perf.spec.ts:302` "delayed worker-link split recovery" pins
`after.fullFrames === before.fullFrames + 1` across a transport resume, and
`apps/coord/tests/terminal-view-owner-mode.test.ts` pins exactly one source-full request for a
replica that cannot seed and zero for a new stream id.

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

**Guard** — `smoke/terminal/terminal-stream-reliability.spec.ts:87` "worker upstream delta loss
obtains a source full on the same browser socket" plus the reveal/deck specs
(`terminal-render-reveal*.spec.ts`, `terminal-render-deck-overlay.spec.ts`).

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

**Guard** — `apps/web/tests/predictiveEchoAck.test.ts` "an echo frame for an earlier keystroke
never contradicts a later one", "a reset re-arms the confidence gate", "an echo that beats the write
ack still unlocks the burst" and "a match that reproduces the cell's own text proves nothing", plus
the real-flow `smoke/terminal/terminal-predictive-echo.spec.ts` "fast typing never paints a
prediction the PTY contradicts".

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

**Guard** — `apps/web/tests/cellRenderer.reconcile.dom.test.ts` "a leading predicted caret does not
freeze reconciliation" and the real-flow `smoke/terminal/terminal-predictive-echo.spec.ts`
"sustained fast typing never wipes its own predictions" (asserts a `resetCount` delta of 0 across a
1.6 s burst).

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

**Guard** — `smoke/terminal/terminal-switch-perf.spec.ts` "the deck mounts a bounded number of
panes" (scoped `targetSlot` locator plus the session-scoped `querySelector` in its repair round)
and `smoke/terminal/terminal-delivery.spec.ts`, which still pins the single-pane stage/percent
series.

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

**Guard** — `apps/web/tests/terminalDirectRegistry.test.ts` "retires every route and candidate for
only the removed worker", `apps/web/tests/localTerminalGrants.test.ts` "removal clears a worker
grant and fences its in-flight mint", `apps/web/tests/machines-delete.dom.test.ts`, and
`smoke/terminal/terminal-peer-failover.spec.ts` "worker deletion retires direct authority before a
held authenticated input reaches the PTY".

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

**Guard** — `apps/web/tests/terminalPeerOwner.test.ts` "retries a transient initial grant failure at
the bounded retry deadline" plus `smoke/terminal/terminal-peer-failover.spec.ts` "worker restart
retires the old peer epoch while its keeper PTY survives" and "active direct grant expiry closes
the peer during coordinator loss and a renewed route stays usable".

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

**Guard** — `apps/roost-cli/tests/deploy-coordinator-release.test.ts`: "a detached coordinator release at its
installed SHA is admitted without any upstream", plus the wrong-checkout, wrong-build, and dirty-tree refusals.

---

## Process rule

When a user-reported symptom matches an entry above, fix at THAT layer first. If the entry describes a
different fix pattern than the one the immediate code tempts you toward, the entry wins — it was written
after the tempting fix already failed. Add a new entry only after a NEW root cause is confirmed AND a
regression test (or a `scripts/lint-roost.ts` rule) exists for it; an entry without a guard is a promise
the repo cannot keep.
