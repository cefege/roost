//! The terminal core: an emulator behind a trait, and the emitter that turns
//! what it holds into the wire's `CellGridFrame`.
//!
//! v2 ran a patched WebAssembly build of the Zig `@wterm/core` library. v3
//! runs `alacritty_terminal` in process, so the emitter, the wire format and
//! the conformance vectors all still describe the same thing and a behaviour
//! difference is a diff against the code it replaces rather than a mystery.
//!
//! **`TerminalCore` is NOT the whole of v2's capability surface, and this
//! header previously claimed it was.** It carried "the trait is the v2
//! WebAssembly ABI's capability surface under the same names", and a slice
//! that read it, believed the surface was complete, and discovered otherwise
//! at the call site. Four members v2's worker callers actually use are still
//! absent: `getResponse` and `writeRaw` (draining and feeding the core's own
//! queued native replies, which is what the query-reply lane needs),
//! `getResourceState` and `synchronizedOutput` (per-cell-sync state, which
//! `stream_fence.rs` and the diagnostics `SyncOutput` gate are written
//! against). The ABI is the *emitter's* contract; v2's callers did not live
//! inside it.
//!
//! **They are absent because the emulator behind the trait cannot offer them,
//! so this trait will not grow them.** `alacritty_terminal` has no raw-byte
//! entry, no reply queue and no per-cell-sync state. A caller that needs one
//! builds it beside the core; it does not belong on a trait whose other
//! implementors are the emitter's reader, which does not answer capability
//! probes and should not have to pretend to. A reader who assumes the trait is
//! merely incomplete will propose adding these four, which is the thing this
//! header exists to prevent.
//!
//! The claim is under-stated rather than over-stated on purpose: a reader who
//! is told a surface is incomplete spends thirty seconds checking, and a
//! reader who is told it is complete and is not loses a wave.
//!
//! Three differences from the core it replaces are stated here rather than
//! discovered later. `alacritty_terminal` has no `CELL_BLINK` flag, so a
//! blinking span stops blinking. It has no byte-input method, so this crate
//! owns a `vte::ansi::Processor` beside the `Term`. And it cannot report how
//! many history lines its ring has discarded, which is the one thing Roost
//! cannot work around — hence the vendored patch in
//! `third_party/alacritty_terminal`, and hence a core that answers
//! `discarded_line_count() -> None` is refused rather than approximated.

#![forbid(unsafe_code)]

pub mod alacritty;
pub mod core;
pub mod emitter;
pub mod error;
pub mod frame;
pub mod row_spans;

pub use alacritty::AlacrittyCore;
pub use core::{CellData, CursorState, TerminalCore};
pub use emitter::{CellEmitState, LIVE_DELTA_SCROLLBACK_ROWS_CAP, next_cell_frame};
pub use error::{TerminalCoreError, TerminalCoreResult};
pub use frame::{grid_delta_frame, grid_to_cell_frame, read_scrollback_range, scrollback_origin};
pub use row_spans::{link_uri_within_cap, row_to_spans};

/// The palette index meaning "the terminal's own default colour".
///
/// Re-exported from `roost_protocol::cell` so a core's cell mapping and the
/// wire's span model cannot disagree about which number that is.
pub use roost_protocol::cell::DEFAULT_COLOR;
