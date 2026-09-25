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
