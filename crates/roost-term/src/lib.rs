//! The terminal core: an emulator behind a trait, and the emitter that turns
//! what it holds into the wire's `CellGridFrame`.
//!
//! v2 ran a patched WebAssembly build of the Zig `@wterm/core` library. v3
//! runs `rio-vt` in process (vendored with Roost's patches,
//! `third_party/rio_vt/ROOST-PATCHES.md`), so the emitter, the wire format and
//! the conformance vectors all still describe the same thing and a behaviour
//! difference is a diff against the code it replaces rather than a mystery.
//!
//! **`TerminalCore` is NOT the whole of v2's capability surface**, and a
//! caller who assumes it is finds out at the call site. The ABI is the
//! emitter's contract; v2's callers did not all live inside it. Two members
//! v2's worker callers use are absent: `getResourceState`
//! and `synchronizedOutput` (per-cell-sync state, which `stream_fence.rs` and
//! the diagnostics `SyncOutput` gate are written against). The emulator cannot
//! offer them, so a caller that needs one builds it beside the core, and a
//! reader who assumes the trait is merely incomplete should not add them.
//!
//! Three v2 members ARE here: `write_raw` and `get_response` (the query-reply
//! lane feeds the core and drains its queued answers — rio's `PtyWrite`
//! events, queued by the adapter), and `unhandled_sequences` (v2's debug ring
//! of dropped CSI — fed by the vendored R6 listener hook). A plain `write`
//! discards replies, so a replay never answers history.
//!
//! The core has no `CELL_BLINK` output, so a blinking span does not blink. It
//! has no byte-input method, so this crate owns a `Processor` beside the
//! `Crosswords`. And a core that answers `discarded_line_count() -> None` is
//! refused rather than approximated: every absolute history index depends on
//! that count.

#![forbid(unsafe_code)]

pub mod core;
pub mod emitter;
pub mod error;
pub mod frame;
pub mod rio;
pub mod row_spans;
pub mod unhandled;

pub use core::{CellData, CoreImagePlacement, CursorState, TerminalCore};
pub use emitter::{CellEmitState, LIVE_DELTA_SCROLLBACK_ROWS_CAP, next_cell_frame};
pub use error::{TerminalCoreError, TerminalCoreResult};
pub use frame::{grid_delta_frame, grid_to_cell_frame, read_scrollback_range, scrollback_origin};
pub use rio::{NOMINAL_CELL_HEIGHT_PX, NOMINAL_CELL_WIDTH_PX, RioCore};
pub use row_spans::{link_uri_within_cap, row_to_spans};
pub use unhandled::{UNHANDLED_RING_CAPACITY, UnhandledSequence, UnhandledSequenceRing};

/// The palette index meaning "the terminal's own default colour".
///
/// Re-exported from `roost_protocol::cell` so a core's cell mapping and the
/// wire's span model cannot disagree about which number that is.
pub use roost_protocol::cell::DEFAULT_COLOR;
