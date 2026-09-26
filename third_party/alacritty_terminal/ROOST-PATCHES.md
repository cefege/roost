# ROOST-PATCHES — vendored `alacritty_terminal` 0.26.0

Upstream version, unmodified except as listed below. Vendored from crates.io
and wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.

**The upstream test suite runs unmodified and must stay green**: 132 unit
tests and 45 reference tests. If a patch broke something those cover, the patch
is wrong. (`tests/roost_discarded_history.rs` is the one added test target,
registered explicitly because the vendored manifest sets `autotests = false`.)

## P1 — a count of history lines the grid has scrolled off the top

### What upstream does

`alacritty_terminal` reports how much history it *retains* —
`Dimensions::history_size()` is `total_lines() - screen_lines()` — and how far
the viewport is scrolled back, via `display_offset()`. Neither says how many
lines have ever scrolled off the top. There is no `discarded`, `trimmed` or
`evicted` counter anywhere in the crate, and no `Event` variant reporting one.

### Why Roost needs it

Roost addresses scrollback by a *monotonic* index: `absolute = sb_origin +
offset`, where `sb_origin` counts every line the history has evicted since the
terminal was created. The emitter that produces a `CellGridFrame` reads that
count to compute `sb_base`, `scrollback_total` and the append range, and to
decide when a delta is no longer truthful and a full reframe is owed.
`packages/protocol/src/cell/emitter.ts` throws rather than guess when the core
cannot report it, and its comment records what the previous inference cost:
re-identifying the previously-newest lines by content hash ran about 1200 WASM
reads per emit near the cap, went blind past a 256-line scan window, and could
alias two identical tails. Each of those failures silently re-aliased absolute
row indices, which is the `docs/FAILURE-INDEX.md` "history mis-splices" class.

The count cannot be derived from outside the crate. Every path that shrinks
history — `Grid::update_history`, `Grid::clear_history`, `Grid::reset`, and
the trim inside `Grid::resize` — is reachable only through those inherent
methods on private fields, so an embedder cannot observe them and subtract.

### The change

Two files, three edits.

`src/grid/mod.rs`
- `Grid<T>` gains a `discarded_line_count: u64` field, initialised to 0 in
  `new`. It carries `#[cfg_attr(feature = "serde", serde(default))]` so a
  reference fixture recorded before the patch still deserializes — without it
  all 45 ref tests fail on `missing field`.
- `Grid::increase_scroll_limit` now returns how many lines it actually granted.
  Its existing clamp to `max_scroll_limit - history_size()` *is* the eviction
  decision; the return value just makes the deficit visible.
- `Grid::scroll_up` takes that return value and advances the counter by
  `positions - granted`: the lines the history could not take.
- `Grid::discarded_line_count(&self) -> u64` exposes it.

`src/term/mod.rs`
- `Term::discarded_line_count(&self) -> u64`, forwarding to the active grid.

### Why the scroll-limit clamp and not the ring arithmetic

The obvious place to count is `Storage::rotate`, where the ring's start offset
wraps. That is wrong, and the guard test is what proved it. The ring is
over-allocated: `Storage::inner` is a `Vec` sized to `len + MAX_CACHE_SIZE` and
is only reallocated when `initialize` outgrows it, so `zero` walks through
hundreds of slack slots without a wrap. Counting there reported **zero** for
1350 lines of scroll against a 100-line cap, while the client's history was in
fact being overwritten the whole time. The clamp in `increase_scroll_limit` is
the site where "there is no room left" is actually decided, and it is the only
site that sees both the request and the room.

### The two semantic decisions, stated because upstream has no opinion

1. **A viewport grow does not count.** Growing the viewport pulls lines *out*
   of history into the view. Those lines are still addressable, so the origin
   must not move. The emitter detects this case itself by watching its own
   monotonic total go backwards (`monoTotal < lastSbTotal` in
   `nextCellFrame`) and reframes. If a grow advanced the counter, the total
   would hold steady and the emitter would ship a delta across a gap.

2. **A viewport shrink into a full history *does* count.** Shrinking the
   viewport pushes its lines into history; when history is already at its cap
   that is a real eviction, and pretending otherwise would mis-name the
   surviving window. Both directions are asserted.

`clear_history` — which a `RIS` escape triggers — does not count either. It
pops lines back toward the viewport rather than losing them, and the emitter
sees the total fall and reframes.

### The invariant a consumer relies on

The retained window is
`discarded_line_count() .. discarded_line_count() + history_size()`. Reading a
history row by its absolute index means mapping it through that window, and a
row outside it is gone rather than merely scrolled.

### How it is pinned

`tests/roost_discarded_history.rs`, 8 cases:

- below the cap: the count is 0 and the monotonic total advances by exactly
  the lines scrolled;
- filled to exactly the cap: the count is 0;
- one line past the cap: the count is exactly 1 — the boundary case that
  separates "trending upward" from "the overflow";
- far past the cap: the count equals the overflow exactly, and the total still
  advances;
- the window moves by exactly one per line and keeps its size;
- a viewport grow does not move the count; a viewport shrink into a full
  history advances it by exactly the number of rows removed;
- `clear_history` does not move the count.

The expectations are derived from the terminal's own reported baseline rather
than hand-counted, because a line feed only scrolls once the cursor is on the
last row — a literal constant in that file would encode an off-by-one that
says nothing about the counter.

## P2 — relative cursor motion is bounded by the DECSTBM margins

`src/term/mod.rs`, `Handler::move_up` and `Handler::move_down`.

Upstream subtracts the count and lets `goto` clamp, and `goto` bounds by the
screen unless origin mode is set. A program that sets margins and then moves
the cursor relative therefore escapes the region it asked for. The Zig core
v2 shipped was patched to the margin rule, and `protocol/conformance/
terminal-core/cursor-margins-clamp.json` pins it — that vector now agrees
with xterm, which it did not before this patch.

The margin logic is transcribed from the two Zig hunks rather than written
fresh, including the `min_row`/`max_row` guards that fall back to the screen
when the cursor is already outside the region.

## P3 — the alternate grid is top-anchored on a shrink

`src/grid/resize.rs`, `Grid::resize` and `Grid::shrink_lines`, plus the two
call sites in `Term::resize`.

Upstream scrolls to keep the cursor visible, which is right for the primary
grid (a viewport onto history) and wrong for the alternate screen: shrinking
an alt grid moves the content the user is looking at. The Zig core was
patched to top-anchor the alt grid, and
`protocol/conformance/terminal-core/alt-grid-survives-a-shrink.json` pins
it.

The anchor follows the ACTIVE grid, not the mode: `grid` is whichever grid
is in use, so it is the alt one exactly when `is_alt` is set. Getting that
backwards is what upstream's own `shrink_lines_updates_inactive_cursor_pos`
test caught during this work.

A top-anchored shrink discards the bottom rows and leaves the cursor past the
new last row, so it clamps the row instead of scrolling. The column is
untouched, matching the Zig core, which only clamps the column when it is
past the last column.

## Not patched: VPA cannot be distinguished from CUD

The v2 patch gives VPA and CUD different rules — `CSI e` routes to
`cursorDownToScreen`, absolute on the screen, while `CSI B` stops at the
bottom margin. `vte` dispatches `('B', [])` and `('e', [])` to the same
`Handler::move_down`, so at that boundary the two sequences are
indistinguishable and only one rule can be implemented.

`protocol/conformance/terminal-core/vpa-ignores-margins.json` therefore
records the behaviour as a known divergence even though the vector passes:
the margins it uses make the two rules agree, so the cursor is right by
accident. `blocked_on: "v3"` says which release is meant to close it, and
closing it means splitting the dispatch in a vendored `vte`.

## P4 — ED clears the viewport in place

`src/term/mod.rs`, `Handler::clear_screen`, the `ClearMode::All` arm.

Upstream's `Grid::clear_viewport` is an optimisation: it walks back from the
last cell and SCROLLS the trailing blank rows away rather than writing blanks
into cells that are already blank. On the primary grid that scroll reaches
history, so a plain `CSI 2J` pushes a line into scrollback that the
reference never created. `protocol/conformance/terminal-core/
clear-and-erase.json` pins the reference behaviour.

This is not an exotic case. Clearing the screen is one of the most common
sequences a program emits, and a client indexing history by monotonic count
would see that index shift on every clear.

The patch keeps upstream's path for the case it was written for — a viewport
scrolled back from the active area, where scrolling to the active area IS the
answer — and only changes the pinned-to-active case to `reset_region(..)`.

My first diagnosis of this divergence was wrong: I recorded it as a pending
wrap being resolved before the CSI dispatch. It is not; alacritty defers the
wrap correctly, and the wrap has nothing to do with it. The vector was right
and the explanation was not, which is why the note now names
`clear_viewport` instead.

## P5 — a delete discards; only a scroll reaches history

`src/grid/mod.rs`, `Grid::scroll_up` (new `to_history` parameter), and
`src/term/mod.rs`, `Handler::scroll_up_relative` (same parameter, threaded
through) and `Handler::delete_lines`.

Upstream implements `CSI M` (delete lines) as a scroll up over the region,
which is right for the cells — they move identically — and wrong for the
history. The line leaving the top of a **scroll** becomes scrollback; the line
leaving the top of a **delete** is discarded. Sharing the implementation without
the distinction sent deleted lines into scrollback.

That is not a cosmetic difference. A client addressing history by a monotonic
index then sees its origin move for lines no sequence ever accounted for, so a
gap appears in history that cannot be explained by a missing frame. The vector
`protocol/conformance/terminal-core/insert-and-delete-lines.json` pins it:
`CSI 2M` at the top of a full-screen region leaves scrollback empty, where the
reference terminal also leaves it empty.

`to_history` is `true` for every genuine scroll — `scroll_up` (the SU family and
newline-at-bottom), the vi-mode scrolls, the resize paths, and
`clear_viewport` — and `false` only for `delete_lines`. The parameter is a
`bool` rather than two methods because the two differ in exactly one branch, and
a second copy of this function would be a second place for the distinction to go
missing.
