//! Where a predicted cell lands: the class it carries, the grid-unit offsets
//! that place it over the pane, and the paint plan the DOM adapter stamps.
//! Offsets are emitted in `ch`/`lh` rather than pixels so a prediction stays on
//! its cell when the pane's font metrics change, which is the same reason the
//! row painter works in grid units. Nothing here reads a DOM, so every
//! placement the overlay can produce is decidable in a test.

use roost_client_core::client::predictive_echo::report::{EchoPaint, PredictedCell};

/// The overlay's own class on the pane's viewport.
pub const OVERLAY_CLASS: &str = "cell-predict";

/// A predicted GLYPH cell: the guess is the character itself.
pub const PREDICTED_GLYPH_CLASS: &str = "cell-predict-ch";

/// A predicted ERASE cell: the default background is painted over one column.
pub const PREDICTED_ERASE_CLASS: &str = "cell-predict-erase";

/// One painted cell, reduced to exactly what an element needs: where it sits,
/// what it says, and which class gives it its colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedPrediction {
    /// The class that distinguishes a guess from an erase.
    pub class_name: &'static str,
    /// The glyph, or empty for an erase cell.
    pub text: String,
    /// The column offset, in grid units.
    pub left: String,
    /// The row offset, in grid units.
    pub top: String,
    /// Whether the guess is underlined, which says "this is a guess on a slow
    /// link". An erase cell never is: it would underline a line the terminal
    /// never drew.
    pub underlined: bool,
}

/// The column offset one cell is painted at.
pub fn cell_left(column: u32) -> String {
    format!("{column}ch")
}

/// The row offset one cell is painted at.
pub fn cell_top(row: u32) -> String {
    format!("{row}lh")
}

/// The inline CSS one painted cell carries, before any element exists.
///
/// `position` is absolute because the overlay is a sibling of the grid rows
/// rather than a cell inside one: a predicted cell must never take part in the
/// row layout it is covering.
pub fn prediction_style(painted: &PaintedPrediction) -> String {
    let mut declarations = format!(
        "position:absolute;top:{};left:{}",
        painted.top, painted.left
    );
    if painted.underlined {
        declarations.push_str(";text-decoration:underline");
    }
    declarations
}

/// Reduce one predictor's paint request to the list of cells to stamp, in the
/// order the predictor emitted them.
pub fn plan_paint(paint: &EchoPaint) -> Vec<PaintedPrediction> {
    paint
        .cells
        .iter()
        .map(|cell| planned_cell(cell, paint.flagged))
        .collect()
}

fn planned_cell(cell: &PredictedCell, flagged: bool) -> PaintedPrediction {
    let erase = cell.ch.is_empty();
    PaintedPrediction {
        class_name: if erase {
            PREDICTED_ERASE_CLASS
        } else {
            PREDICTED_GLYPH_CLASS
        },
        text: cell.ch.clone(),
        left: cell_left(cell.col),
        top: cell_top(cell.row),
        underlined: !erase && flagged,
    }
}
