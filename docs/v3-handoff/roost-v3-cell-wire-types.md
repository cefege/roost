# Agreed `roost-protocol` cell wire types (Phase 1)

Settled with the porting agents so the chunk family and the cell-value family
compile against one shape. This is a note for the next agent that touches
these types, not a second source of truth — the code is the source of truth.

## `cell::types`

```rust
pub struct CellSpan {
    pub text: String,
    pub fg: u16,
    pub bg: u16,
    pub flags: u16,
    pub fg_rgb: Option<u32>,
    pub bg_rgb: Option<u32>,
    pub columns: u32,
    pub link_uri: Option<String>,
    pub link_key: Option<String>,
}

pub struct CellRow {
    pub index: u32,
    pub spans: Arc<[CellSpan]>,
}

pub enum MouseTracking {
    None,          // 0
    PressRelease,  // 1000
    ButtonMotion,  // 1002
    Unknown(u32),
}

pub struct CellGridFrame {
    // u32: cols, rows, cursor_row, cursor_col
    // u64: scrollback_total, sb_base, base_seq, seq
}
```

`CellRow::spans` is an `Arc<[CellSpan]>` on purpose. The TypeScript pins
`clone.viewportRows[0].spans === frame.viewportRows[0].spans` — a clone
shares its span arrays rather than copying them, which is what makes cloning a
10k-row frame cheap. Construct a row with `Arc::from([span])`,
`Arc::from(vec)`, or `Arc::from(Vec::<CellSpan>::new())` for an empty row.

`MouseTracking` converts both ways with `From<u32>` and
`From<MouseTracking> for u32`, and `as_mouse_tracking(raw: u32) -> MouseTracking`
mirrors `cell-proto.ts`'s mapping: anything that is not 1000 or 1002 becomes
`None`, so a mode the port has never seen still decodes.

## `viewport`

`TERMINAL_MAX_COLS` and `TERMINAL_MAX_ROWS` are `u32`, so
`CELL_GRID_SNAPSHOT_MAX_ROWS = TERMINAL_MAX_ROWS` compares directly against
`frame.rows` with no cast.
