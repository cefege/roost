//! Semantic marks on a row, from the shell's OSC 133 integration, and the two
//! `CellRow` constructors that carry them.
//!
//! The worker's `roost-term` stamps these bits on grid cells; a row's mark is
//! the OR of its cells'. The browser's prompt gutter, prompt jump and "copy last
//! command output" read them, so the bit values are wire protocol.

use std::sync::Arc;

use super::types::{CellRow, CellSpan};

/// The shell started drawing a prompt on this row.
pub const PROMPT: u8 = 1;
/// A command's output starts on this row.
pub const OUTPUT: u8 = 2;
/// The command run at the PREVIOUS prompt exited 0; carried by the prompt that
/// follows it, as starship and powerlevel10k colour their prompt.
pub const EXIT_OK: u8 = 4;
/// The command run at the previous prompt exited non-zero.
pub const EXIT_FAILED: u8 = 8;
/// All mark bits this version understands.
pub const KNOWN: u8 = PROMPT | OUTPUT | EXIT_OK | EXIT_FAILED;

impl CellRow {
    /// A row without semantic marks.
    pub fn new(index: u32, spans: Arc<[CellSpan]>) -> Self {
        Self {
            index,
            spans,
            mark: 0,
        }
    }

    /// A row with its cells' marks already folded into one value; bits this
    /// version does not know are dropped rather than passed on.
    pub fn with_mark(index: u32, spans: Arc<[CellSpan]>, mark: u8) -> Self {
        Self {
            index,
            spans,
            mark: mark & KNOWN,
        }
    }
}
