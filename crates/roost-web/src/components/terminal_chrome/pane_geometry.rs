//! The pane composer's growth measurement: how far the dock has overflowed
//! ABOVE its resting row, so the pane can translate its display instead of
//! taking rows from it.
//!
//! This is the whole of the "transient chrome never changes the terminal grid"
//! rule on the desktop side. Every PTY height change makes an inline agent TUI
//! repaint, and a TUI that repaints in place duplicates the rows a shrink
//! pushed into history — so a two-line draft must push the TERMINAL up, never
//! shrink it. The dock keeps exactly one row in flex flow and everything past
//! that floats above it, measured here.
//! Ports `apps/web/src/components/terminal/TerminalComposePaneGeometry.ts`,
//! including its `--term-chat-pane-rest` property and its `data-size-constrained`
//! switch, which is what stops an over-long draft from floating out of the pane.

/// How far the dock has grown above its resting row, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneGrowth {
    /// Pixels of overflow above the resting row. Zero when the dock is exactly
    /// its resting height, and zero when it is constrained.
    pub growth_px: u32,
}

/// The measurements one update reads, and the two decisions it makes.
///
/// Separated from the DOM so the arithmetic — which is where the defect lives —
/// is testable without a browser. The DOM half is [`PaneGeometryProbe`], which
/// supplies these numbers from a live dock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneMeasurement {
    /// `box.offsetHeight`: the pill's laid-out height, including its field.
    pub box_height: f64,
    /// `input.offsetHeight - minHeight`: the part of the box the field's own
    /// growth accounts for, so the resting row can be measured without the
    /// draft that is currently in it.
    pub field_growth: f64,
    /// How far the dock's top edge sits above the box's top edge. Positive
    /// only when the box has overflowed upward.
    pub overflow_above: f64,
    /// Whether the dock's content no longer fits: it scrolls horizontally,
    /// exceeds its own height, or would grow past the pane's bottom edge.
    pub content_overflows: bool,
    /// The dock's content height, children plus their row gaps.
    pub content_height: f64,
    /// The room between the dock's bottom edge and the pane's top edge.
    pub room_to_pane_top: f64,
    /// `dock.clientHeight`: the height the dock actually reserves in the pane's
    /// flex flow, which the CSS pins to the resting row.
    pub dock_height: f64,
}

impl PaneMeasurement {
    /// The resting height the dock keeps in flex flow.
    ///
    /// `box.offsetHeight` minus the field's own growth, rounded: the pill's
    /// controls and padding with a one-line field. This is the number the CSS
    /// consumes as `--term-chat-pane-rest`, and it is what keeps a long draft
    /// from becoming a taller flex child.
    #[must_use]
    pub fn resting_height(&self) -> f64 {
        (self.box_height - self.field_growth).round().max(0.0)
    }

    /// Whether the dock has to stop floating and scroll instead.
    ///
    /// Two independent measures, folded into one flag so the mode cannot flap
    /// between passes: the content no longer fits what the dock reserves, or
    /// the dock's own resting row no longer fits inside the height the dock
    /// reserves. The second is what a dock whose controls outgrew the published
    /// resting row looks like, and floating it further would push the field out
    /// of the pane entirely.
    #[must_use]
    pub fn constrained(&self) -> bool {
        self.content_overflows || self.resting_height() > self.dock_height + 1.0
    }

    /// What the dock publishes, given this measurement.
    ///
    /// A constrained dock grows ZERO and scrolls instead: a dock floating past
    /// the pane's bottom edge is a composer the user cannot see the end of,
    /// and one that has scrolled horizontally is a composer whose field is
    /// already too narrow to write in.
    #[must_use]
    pub fn publish(&self) -> PaneGrowth {
        if self.constrained() {
            return PaneGrowth::default();
        }
        PaneGrowth {
            growth_px: self.overflow_above.max(0.0).round().max(0.0) as u32,
        }
    }

    /// The dock's `data-size-constrained` value: present only while the dock
    /// is constrained. The stylesheet keys on the attribute's PRESENCE, so an
    /// unconstrained dock writing `"false"` would still be switched into its
    /// scrolling mode — a box laid out downward from the dock's top edge and
    /// clipped by it, with the pane cutting off the rest.
    #[must_use]
    pub fn constrained_attribute(&self) -> Option<&'static str> {
        self.constrained().then_some("true")
    }
}

/// The live DOM readings one update needs. Implemented against a mounted dock.
pub trait PaneGeometryProbe {
    /// Everything one update reads, or `None` when an element is not laid out
    /// yet — a zero-width dock cannot be measured and must not be guessed at.
    fn measure(&self) -> Option<PaneMeasurement>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resting() -> PaneMeasurement {
        PaneMeasurement {
            box_height: 52.0,
            field_growth: 0.0,
            overflow_above: 0.0,
            content_overflows: false,
            content_height: 52.0,
            room_to_pane_top: 600.0,
            dock_height: 52.0,
        }
    }

    #[test]
    fn a_dock_at_its_resting_height_publishes_no_growth() {
        assert_eq!(resting().publish(), PaneGrowth::default());
    }

    #[test]
    fn the_resting_row_excludes_the_growth_the_field_itself_caused() {
        // A three-line draft: the box is 52 + 2 extra rows, and the field's own
        // growth is exactly those two rows, so the resting row is unchanged.
        let measured = PaneMeasurement {
            box_height: 52.0 + 2.0 * 18.0,
            field_growth: 2.0 * 18.0,
            ..resting()
        };
        assert_eq!(measured.resting_height(), 52.0);
    }

    #[test]
    fn a_draft_above_its_resting_row_publishes_exactly_that_overflow() {
        let measured = PaneMeasurement {
            overflow_above: 36.4,
            ..resting()
        };
        assert_eq!(measured.publish().growth_px, 36);
    }

    #[test]
    fn a_constrained_dock_grows_nothing_and_scrolls_instead() {
        // The dock's content no longer fits the pane it is in, so floating it
        // further up would put the field outside the pane.
        let measured = PaneMeasurement {
            overflow_above: 400.0,
            content_overflows: true,
            content_height: 900.0,
            room_to_pane_top: 120.0,
            ..resting()
        };
        assert_eq!(measured.publish(), PaneGrowth::default());
    }

    #[test]
    fn a_dock_that_has_not_been_laid_out_measures_nothing_rather_than_zero() {
        // The zero case is the dangerous one: a dock with no width yet reads a
        // height of 0, and publishing 0 would un-grow a composer that is
        // actually tall. `None` says "ask again", which is what a mount does.
        struct Unlaid;
        impl PaneGeometryProbe for Unlaid {
            fn measure(&self) -> Option<PaneMeasurement> {
                None
            }
        }
        assert_eq!(Unlaid.measure(), None);
    }

    #[test]
    fn a_negative_overflow_means_the_box_is_below_the_dock_and_publishes_nothing() {
        // The status line grew DOWNWARD into the pane rather than the field
        // overflowing upward. That is still a growth, but it is the flex row's
        // business, not an upward float.
        let measured = PaneMeasurement {
            overflow_above: -12.0,
            ..resting()
        };
        assert_eq!(measured.publish().growth_px, 0);
    }

    #[test]
    fn a_resting_row_too_tall_for_its_own_dock_constrains_it() {
        // The dock reserves what it published as the resting row, so the row
        // that fits by construction fits the dock too — until the pill's own
        // controls outgrow the row they were measured against. Floating that
        // dock would put the field outside the pane; it has to scroll instead.
        let measured = PaneMeasurement {
            box_height: 88.0,
            overflow_above: 36.0,
            content_height: 88.0,
            dock_height: 52.0,
            ..resting()
        };
        assert!(measured.constrained());
        assert_eq!(measured.publish(), PaneGrowth::default());
    }

    #[test]
    fn a_draft_that_only_grew_the_field_is_not_constrained_by_its_own_height() {
        // The whole point of the resting row: five lines of draft raise the box
        // and raise the growth with it, and neither is a constraint.
        let measured = PaneMeasurement {
            box_height: 52.0 + 4.0 * 18.0,
            field_growth: 4.0 * 18.0,
            overflow_above: 72.0,
            content_height: 52.0 + 4.0 * 18.0,
            dock_height: 52.0,
            ..resting()
        };
        assert!(!measured.constrained());
        assert_eq!(measured.publish().growth_px, 72);
    }

    #[test]
    fn an_unconstrained_dock_carries_no_constrained_attribute_at_all() {
        // The stylesheet matches `[data-size-constrained]` by presence, so a
        // written "false" would put a growing draft into the scrolling mode.
        let grown = PaneMeasurement {
            box_height: 52.0 + 7.0 * 24.0,
            field_growth: 7.0 * 24.0,
            overflow_above: 7.0 * 24.0,
            content_height: 52.0 + 7.0 * 24.0,
            ..resting()
        };
        assert_eq!(resting().constrained_attribute(), None);
        assert_eq!(grown.constrained_attribute(), None);
    }

    #[test]
    fn a_constrained_dock_carries_the_attribute() {
        let measured = PaneMeasurement {
            content_overflows: true,
            ..resting()
        };
        assert_eq!(measured.constrained_attribute(), Some("true"));
    }
}
