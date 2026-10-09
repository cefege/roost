# ROOST-PATCHES — vendored `rio-vt` 0.5.28

crates.io `rio-vt` 0.5.28 (MIT, github.com/raphamorim/rio), the terminal core
behind `roost-term`. Unmodified except as listed below (R1–R7). Wired in through
`[patch.crates-io]` in the workspace `Cargo.toml`; outside the workspace, so
its suite runs by manifest path:

```
cargo test --manifest-path third_party/rio_vt/Cargo.toml --no-default-features --features graphics
```

**The upstream suite (554 unit tests) runs unmodified and must stay green.**
The `tests/roost_*.rs` targets are the guards on the patches below,
registered as `[[test]]` entries because the published manifest sets
`autotests = false`. `rustfmt.toml` is upstream rio's, verbatim; the crates.io
package ships without it, and each hunk below is formatted with it.

Every hunk carries a `ROOST PATCH Rn` comment, so `grep -rn 'ROOST PATCH'
src` lists them.

## What upstream already covers

Roost's previous core (`alacritty_terminal` 0.26 with patches P1–P10) needed a
count of history lines lost off the top (P1): rio's `Crosswords::lines_evicted`
is that count — it advances on a cap eviction, a shrink overflow and
`clear_history` (the last of which P1 did not count; `roost-term`'s emitter
reframes when the count jumps by more than the lines it appended). The
keyboard-mode stack fixes (P8–P10) are already correct in rio. Not carried:
P3, the top-anchored alternate grid on a shrink —
`protocol/conformance/terminal-core/alt-grid-survives-a-shrink.json` records
rio's scroll-to-keep-the-cursor behaviour as a known divergence.

## R1 — LF clears a pending wrap

`src/crosswords/mod.rs`, `Handler::linefeed`, one line before the scroll
decision.

Upstream clears `grid.cursor.should_wrap` on `goto`, `carriage_return` and
`backspace`, not on `linefeed`. xterm clears it. A pending wrap is spent by
the next printable cell, so an LF that leaves it armed makes the cell after
it wrap into the row below the one the LF moved to; a `CUU` that follows
moves up one, not two, and an in-place status rewrite walks downward and
scrolls once per generation (`docs/FAILURE-INDEX.md` "A fast in-place row
rewrite duplicates rows into history").

Guard: `tests/roost_linefeed_pending_wrap.rs`.

## R2 — a delete discards; only a scroll reaches history

`src/crosswords/grid/mod.rs`, `Grid::scroll_up_to` (the body of
`Grid::scroll_up`, which forwards `to_history = true`), and
`src/crosswords/mod.rs`, `Crosswords::scroll_up_relative_to` (the body of
`scroll_up_relative`, same forwarding) and `Handler::delete_lines`, the only
caller that passes `false`.

Upstream implements `CSI M` as a scroll up over the region from the cursor.
When the cursor is on row 0 of a full-screen region that is a full-screen
scroll, and the deleted lines enter scrollback. The reference terminal
discards them. A client addressing history by monotonic index would see its
origin move for lines no sequence accounted for.
`protocol/conformance/terminal-core/insert-and-delete-lines.json` pins it.
With `to_history == false` the region rotates without touching history,
`lines_evicted`, the display offset or the vi cursor's viewport anchor, and
sixel/iTerm2 placements shift with their content as for any sub-region
scroll.

Guard: `tests/roost_delete_lines_discard.rs`.

## R3 — ED clears the viewport in place

`src/crosswords/mod.rs`, `Handler::clear_screen`, the `ClearMode::All` arm.

Upstream's `Grid::clear_viewport` scrolls the trailing rows away rather than
blanking them, and on the primary grid that scroll reaches history: a plain
`CSI 2J` pushes lines into scrollback the reference never creates, shifting
every monotonic history index. When the viewport is pinned to the active area
(`display_offset == 0`) the patch resets the viewport rows in place and clips
sixel/iTerm2 placements on screen, as the alternate-screen arm already does.
A viewport scrolled back keeps upstream's path.
`protocol/conformance/terminal-core/clear-and-erase.json` pins it.

Guard: `tests/roost_clear_in_place.rs`.

## R4 — relative cursor motion is bounded by the DECSTBM margins

`src/crosswords/mod.rs`, `Handler::move_up` and `Handler::move_down`.

Upstream subtracts the count and lets `goto` clamp to the screen, so a
program that sets margins and moves relative escapes the region it asked
for. The rule is transcribed from Roost's alacritty patch P2 (itself from the
v2 Zig core), including the fallback to the screen edge when the cursor is
already outside the region. `CSI B` and `CSI e` (VPR) share `move_down` in
rio as they did in `vte`, so VPR is bounded by the margins too;
`protocol/conformance/terminal-core/vpa-ignores-margins.json` records that
divergence. `protocol/conformance/terminal-core/cursor-margins-clamp.json`
pins the rule.

## R5 — a Roost row mark on `Row`

`src/crosswords/grid/row.rs`: `Row::roost_mark: u8`.

Roost marks OSC 133 prompt, output and exit-status boundaries per row (bits
PROMPT=1, OUTPUT=2, EXIT_OK=4, EXIT_FAILED=8, from
`crates/roost-protocol/src/cell/row_mark.rs`). Rio's own `semantic_prompt`
has a different vocabulary and no exit status, and nothing outside the crate
can attach data to a row that travels with it through scrollback rotation and
reflow. The field is set, copied and reset at exactly the sites
`semantic_prompt` is: `new`/`default`/`from_vec` start it at 0, `copy_from`
copies it, `reset` and `recycle` zero it. A row keeps it through history
rotation and reflow; a continuation row split off by a narrowing reflow
starts unmarked.

Guard: `tests/roost_row_mark.rs`.

## R6 — a hook for dropped CSI sequences; input noise logs at debug

`src/event/mod.rs` (`EventListener::unhandled_csi`, default no-op),
`src/performer/handler.rs` (`Handler::unhandled_csi`, called from the
`csi_unhandled!` macro) and `src/crosswords/mod.rs` (`Crosswords` forwards to
its listener).

Upstream reports a CSI it drops only as a `warn!` line. Roost surfaces dropped
sequences as the `terminal.unhandled_sequence` diagnostic, and the listener is
the only object an embedder owns inside the parse. The hook receives the final
byte, the `<`..`?` private marker (or 0), the parameter count and the first
sub-parameter of up to four parameters. An unknown SGR attribute inside an
otherwise applied `CSI … m` is not reported (the rest of the SGR applied).

Every `warn!` in `src/performer/` reported program input (an unknown CSI,
ESC, OSC, APC or C0, a malformed kitty/glyph/sixel payload, a repeat with no
preceding char, a non-local OSC 7 host). Those are now `debug!`: a program
printing garbage must not write to the worker's error log.

Guard: `tests/roost_unhandled_csi.rs`.

## R7 — `simdutf` is an optional feature

`Cargo.toml` (`simdutf` made optional, a `simdutf` feature in `default`),
`src/simd_utf8.rs`, `src/simd_base64.rs` and `src/performer/parser/mod.rs`
(every `cfg(target_arch = "wasm32")` split that chose between simdutf and
the scalar path now also takes the scalar path when the feature is off).

`simdutf` is a C++ library compiled by the crate's build script. Its AVX-512
kernels do not build with the C toolchain Roost's Linux x64 release job uses
(`'_mm512_set1_epi32' requires target feature 'evex512'`), which failed the
v3.0.0-rc.18 release. Roost depends on rio-vt with `default-features = false`,
so it takes upstream's own scalar path — the one upstream ships on wasm32 —
on every target, and no C++ is compiled. The scalar base64 engine also
forgives non-zero trailing bits, as simdutf's Loose mode does, so a kitty
payload a simdutf build draws is drawn here too. The one upstream test that pins
simdutf's own error-length convention for a UTF-8-encoded surrogate (3, where
std's validator reports the maximal subpart, 1) runs only with the feature.

Guard: `tests/roost_scalar_base64.rs`, and the suite runs both ways, `--no-default-features --features graphics`
(what Roost builds) and `--features graphics,simdutf`.
