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

### Alt-screen wallpaper of stale text after a worker restart

**Symptom** — "after worker restart an alternate-screen session shows wallpaper of stale text + overlapping/parallel lines"

**Wrong** — `resume()` rebuilds an empty wtermCore + records alternate-screen state, but the serializer reads
`core.usingAltScreen()` (false on an empty core) → fresh snapshot omits `ESC[?1049h` → live alt redraws land in
main-screen.

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
the worker count); `GetHistory`/`GetHistoryResp` frames are additive with NO version bump so they can't trip
killStaleKeeper; `resume()` re-reads via the pool's history call
(`apps/worker/src/keeper/keeper-pool-channels.ts`) and seeds `scrollback`+`head_seq`. A pre-RC2 keeper hits a 3s
timeout → graceful fallback. Activates only on keeper REPLACEMENT (reboot), not a plain worker kickstart.

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
`apps/worker/tests/terminal-stream-state.test.ts`;
`apps/shared/tests/wterm-resize-in-place.test.ts`;
`smoke/terminal/terminal-multiview.spec.ts`.

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

### Scroll position lurches — many writers of scrollTop

**Symptom** — "terminal scrollback jumps around / view lurches while scrolling up / lands mid-history after a tab switch / drifts off the bottom after vim/less/claude exits"

**Wrong** — row-space or pixel scroll ownership: intent/anchor state, distance compensation, scroll-event
classification, resize/reveal correction, or a jump-to-bottom control.

**Right** — **exact pre-mutation bottom check plus ONE conditional writer.** `CellGridRenderer`
(`apps/web/src/lib/cellRenderer.ts`) captures `scrollTop >= max(0, scrollHeight - clientHeight)` before a
painted-height mutation; only `_pinToBottom(wasAtBottom)` may assign `scrollTop`, and only when that captured
value was true. Non-bottom mutations never write position. The mutable append tail is `overflow-anchor:none` so
Chromium does not follow it when the reader is one pixel above bottom; completed blocks are anchors, and the
tail is restored before a backfill prepend so native anchoring preserves the reader's row. Never restore intent
state, add reveal correction, or a jump-to-bottom control. This single-writer invariant is about POSITION and
presumes the scroll SPACE is truthful — the spacer entry below is what makes it so.

**Guard** — `apps/web/tests/` renderer DOM suite —
`"a non-bottom backfill prepend performs no application scroll write"`,
`"only the mutable tail is excluded from browser anchoring"`,
`"unchanged and fully clamped pins leave no stale scroll ownership"`,
`"a coalesced pin retargets once, then the next native scroll reads"`.

### A parked pane paints at a lying box size

**Symptom** — "tab switch shows stale terminal content / a returned-to pane sits above the live bottom and never follows output again / bottom-follow works foreground but dies after a park"

**Wrong** — latch the bottom in intent state, correct scroll at reveal, add an `atBottom()` tolerance, or defer
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
`"a backfill prepend shrinks the spacer by exactly the rows it adds"`,
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

**Wrong** — bridge the claim snapshot's tail back to the viewer's held boundary (a 2000-row catch-up), or
replace that with a constant 250-row tail plus a reveal-triggered proactive refill — either form puts retained
history work ahead of or immediately behind the current viewport, scales resume work with session depth, and
mutates the painted grid without reader demand; equally wrong: racing history away or reordering a mixed
history+viewport repaint (breaks the single scroll writer). Measured: a 2000-row catch-up frame is 324–516 KiB
of proto, 38 ms of blocked worker event loop, and ~300 ms of scrollback DOM built BEFORE the viewport paints.

**Right** — **every authoritative FULL frame is viewport-only and epoch-addressed.** It carries no scrollback
rows, a base equal to the total, and an opaque `gridEpoch`; the renderer immediately replaces the current
viewport and the truthful spacer, then issues zero history RPCs while the reader remains at bottom. Only
explicit scroll/find demand fetches disjoint `SessionsGetScrollbackCells` ranges carrying that epoch
(`apps/coord/src/connect/handlers-sessions-scrollback.ts` relays it); the worker checks the epoch before and
after each cooperative slice and returns an error rather than splice re-numbered rows. While the reader is
off-bottom, every FULL frame — including an epoch change — is retained off-DOM as the latest pending frame and
deltas fold into it; the painted frame, spacer, `scrollTop` and visible row stay immutable until an explicit
return to bottom applies the latest frame once. If the worker ring dropped the requested prefix, the shorter
response's start row is the retained floor: paint the surviving suffix and park there rather than rejecting the
page or re-requesting impossible rows. Paused Sync recovery resumes the mounted loop in place (no reload), and
durable replay yields periodically so live cells preempt it.

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

- (c) `TerminalViewHub` is the only membership/SCD owner. It independently
  minimizes active columns and rows, parks disconnected sockets only until
  their existing lease expires, and mints a UUID stream for every effective
  geometry or worker-generation transition. Invalid or fail-closed worker
  outcomes retain membership but publish unavailable until route
  reconciliation can issue a fresh stream.

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

**Wrong** — chase the 502 into tailscale-serve, restart the worker, or read the SPA's host metrics and conclude
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
tailscale serve keeps the worker-side TCP ESTABLISHED, so `ws.onerror`/`ws.onclose` NEVER fire and `ws.send`
(including in-band JWT refresh) black-holes forever; the restarted coord's in-memory `connectWorkers` registry
has no WS for the fingerprint → the hub socket lookup returns null →
`apps/coord/src/connect/handler-session-spawn.ts` throws failed_precondition on every spawn while heartbeats (a
separate unary transport) keep the row looking alive.

**Right** — **a stale-link watchdog on the worker side** in `apps/worker/src/transport/coord-link.ts`
(`dial()`'s open/message handlers): coord pings every 30s (`apps/coord/src/connect/worker-conn.ts`); every
downstream frame stamps `lastDownstreamAtMs`; a per-dial interval (`STALE_CHECK_INTERVAL_MS` 15s) force-closes
and re-dials after `STALE_LINK_TIMEOUT_MS` 90s (3 missed pings) of downstream silence → hello→snapshot replay
heals the rest. Same half-open-through-tailscale class as the boot RPC timeout.

**Guard** — `apps/worker/tests/coord-link-stale-watchdog.test.ts`.

### Cold start loses the event published between snapshot and socket

**Symptom** — "brand-new browser: spawning a terminal does nothing — no pane, no sidebar row, store `sessions` stays empty until a reload / 'works on the second load'"

**Wrong** — dial the Sync socket after the bootstrap lists again (throws away the cold-start win for every warm
boot to fix only the first-ever boot), or paper over it with a post-bootstrap `sessionsList` refetch.

**Right** — **the snapshot must be ordered AFTER the socket is subscribed, and an authorization must wake the
backoff.** coord runs NO backfill from zero, so an event published between `sessionsList` resolving and the
socket subscribing is lost outright — there is nothing to replay it from. (a)
`apps/web/src/store/sync-bootstrap.ts` awaits the subscribed barrier (`waitForSyncSubscribed` in
`apps/web/src/store/sync.ts`, which resolves only once the subscription establishes
socket/domain generations, never at `WebSocket.onopen`) and takes its snapshot against that socket's id, so the
window is CLOSED, not merely shrunk. (b) after a first-boot `_attemptSelfRegister()` authorizes the key,
`resumeSyncNow()` re-dials at once instead of serving out a 1s/2s/4s backoff. Any change to the dial ordering
MUST keep both. Instrumented proof: lists 401 → self-register → retry OK, but the socket's first dial 401'd and
the `opened` event fired during its 1s backoff.

**Guard** — `smoke/terminal/` — `"browser smoke flow creates and cleans its resources"` on a FRESH context;
`apps/web/tests/sync-flow.test.ts` — `"repeated bootstrap retries retain one infinite-loop owner"`.

---

## Coordinator RPC, audit and data integrity

### A fresh browser's key is not authorized yet

**Symptom** — "browser 401 on workers.list after fresh context"

**Wrong** — manual debugging, or a retired REST/tRPC authorize route.

**Right** — the Connect `AuthAuthorizeBrowser` RPC (loopback-or-tailnet,
`apps/coord/src/connect/handlers-auth.ts`) takes the pubkey: `roost api <verb>` SELF-authorizes its own key on
`Unauthenticated` (the bootstrap hook in `apps/roost-cli/src/api.ts`); a fresh BROWSER's IndexedDB WebCrypto
pubkey goes through the same RPC or the loopback pair flow (`PairApprove`).

**Guard** — `apps/coord/tests/public-surface.test.ts` (pins which auth routes are reachable unauthenticated).

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
`apps/coord/src/middleware/rate-limit.ts`: auth (AuthorizeBrowser/MintBootstrap/RedeemWorker/RedeemBrowser),
workspace create/update/delete/set-sessions, task enqueue/set-state/cancel, webhook-token / permission / MCP
mutations, worker rename/delete/deploy-start. `*List`, identity and health probes are NOT in the set.

**Guard** — `apps/coord/tests/coord-e2e.test.ts` —
`"rate limit: 100 AuthAuthorizeBrowser POSTs from same IP → 101st returns 429"`.

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

---

## Product boundaries and process

### Roost never owns the agent session

**Symptom** — "omp launcher stops opening in a normal terminal"

**Wrong** — spawn the agent CLI as a headless child, vendor its runtime, or import an agent browser UI.

**Right** — the agent CLI runs as an ordinary command in a normal shell PTY; Roost transports that terminal and
never spawns, supervises, or owns the agent session.

**Guard** — `none`.

### Redesigns discard previous fixes

**Symptom** — "sidebar redesign loses every previous fix"

**Wrong** — "phase-N: complete sidebar rewrite".

**Right** — additive commits behind a flag; the smoke flow must still pass after each.

**Guard** — `smoke/terminal/terminal-delivery.spec.ts` `"browser smoke flow creates and cleans its
resources"` re-runs the whole flow on every CI run (`runFlow`: workspace create → terminal open → PTY
marker round-trip → pane close → cascade-delete), plus the deck-persistence cases in
`smoke/terminal/terminal-render-deck.spec.ts`. Gap: nothing asserts that a *named* earlier fix
survived a rewrite — only that the flow, the deck, and the `scripts/lint-roost.ts` sidebar rules hold.

---

## Process rule

When a user-reported symptom matches an entry above, fix at THAT layer first. If the entry describes a
different fix pattern than the one the immediate code tempts you toward, the entry wins — it was written
after the tempting fix already failed. Add a new entry only after a NEW root cause is confirmed AND a
regression test (or a `scripts/lint-roost.ts` rule) exists for it; an entry without a guard is a promise
the repo cannot keep.
