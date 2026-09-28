//! The reserved pixel height of one block or gap of scrollback rows, and the
//! two row-height constants every scroll offset in the renderer derives from.
//!
//! Called by the scrollback layout (`cell_renderer/scrollback.rs`) to size an
//! exact-height placeholder, by the font-settled repair, and by the smoke
//! harness and the incident scanner, which name the function directly.
//!
//! It is pure arithmetic: an absolute row index is turned into a scroll offset
//! by multiplying by this height, so the value must be a BARE length. The
//! self-correcting `contain-intrinsic-size: auto <len>` form makes a browser
//! reuse a block's last RENDERED size, which understates `scrollHeight` for a
//! block that grew while skipped. Ports `SCROLLBACK_BLOCK_ROWS`,
//! `DEFAULT_CELL_ROW_PX` and `blockPlaceholder` of
//! `apps/web/src/renderer/cellRendererDom.ts`.

/// Rows one sealed scrollback block holds. A block is the eviction unit and the
/// backfill page size, so both numbers are this one constant.
pub const SCROLLBACK_BLOCK_ROWS: u32 = 250;

/// Row height used before a real measurement exists: a font that has not
/// settled, a pane that has never painted. Every derived offset multiplies this
/// rather than zero, because a zero height collapses the whole scroll space.
pub const DEFAULT_CELL_ROW_PX: f64 = 16.8;

/// The exact contain-intrinsic-size value for a measured block of rows.
///
/// `row_height` falls back to the default when no measurement exists, so a
/// placeholder never collapses to `0px` and a scroll offset derived from it
/// never lands outside its own row.
pub fn block_placeholder(rows: u32, row_height: f64) -> String {
    let height = if row_height > 0.0 {
        row_height
    } else {
        DEFAULT_CELL_ROW_PX
    };
    fixed_px(f64::from(rows) * height)
}

/// A pixel length exactly as v2 stamps it: `` `${value.toFixed(2)}px` ``.
///
/// `toFixed` rounds an exact two-decimal tie away from zero where `{:.2}`
/// rounds it to even. A double is such a tie only when eight times it is an
/// odd integer (`x.125`, `x.375`, `x.625`, `x.875`), and a browser's 1/64-px
/// layout unit makes that an ordinary row pitch — 24 rows of a 16.796875px
/// line are `403.125`, which v2 stamps as `403.13px`.
pub(crate) fn fixed_px(value: f64) -> String {
    let magnitude = value.abs();
    let eighths = magnitude * 8.0;
    if magnitude < 1e15 && eighths.fract() == 0.0 && eighths % 2.0 == 1.0 {
        let hundredths = (magnitude * 100.0).ceil();
        let whole = (hundredths / 100.0).floor();
        let sign = if value.is_sign_negative() { "-" } else { "" };
        return format!("{sign}{whole:.0}.{:02.0}px", hundredths - whole * 100.0);
    }
    format!("{value:.2}px")
}
