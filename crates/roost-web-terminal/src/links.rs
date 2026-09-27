//! Painted terminal links: the attributes the row painter stamps, the scan that
//! turns those attributes back into links, and the activation that turns a
//! scanned link into the action a click performs.
//!
//! `links/scan.rs` and `links/activation.rs` are pure and `#[test]`-covered;
//! `links/dom.rs` is the `wasm32` adapter that reads the attributes off the
//! painted DOM and writes them back. A link is one the core authored, per cell,
//! so this crate never re-derives a link from link TEXT — the class, the run key
//! and the target are all the renderer already stamped.
//!
//! Depends on `cell_row` for the attribute names and on `link_target` for the
//! one classifier; neither is reimplemented here. Ported from v2's
//! `apps/web/src/renderer/terminal-links*.ts`.

pub mod activation;
pub mod scan;

#[cfg(target_arch = "wasm32")]
pub mod dom;

pub use activation::{
    LinkActivation, LinkActivationGesture, LinkArmedHold, LinkModifierKey, PressWithheld,
    activate_link, is_link_activation_gesture, is_worker_file_href, link_hint, link_title,
    withhold_press,
};
pub use scan::{
    DIRTY_ROW_LIMIT, LinkHalf, ScanRequest, ScanSchedule, ScannedLink, link_at_cell, region_links,
    row_links,
};
/// The link attributes one painted anchor carries.
///
/// Every field is optional because the DOM read is: a link painted before the
/// current build, or by an inferred match, carries a target and no run key, and
/// a run key with no target names a link nothing can open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaintedLinkAttributes {
    /// The element carries `TERMINAL_LINK_CLASS`. An anchor without it is not a
    /// terminal link however it was painted, so a scan skips it.
    pub is_terminal_link: bool,
    /// `LINK_KEY_ATTR`: the core's per-run identity. Soft-wrapped halves of one
    /// link carry the SAME key, which is what re-identifies them as one link
    /// after they land in two different rows.
    pub key: Option<String>,
    /// `TERMINAL_LINK_TARGET_ATTR`, else the anchor's own `href` for a link that
    /// already carries a resolved route.
    pub target: Option<String>,
}

/// One painted child of a terminal row: a run of cells, or a link anchor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaintedChild {
    /// The child's GRID OCCUPANCY, which is what a point is tested against. A
    /// cell is neither a character nor a code unit, so this is the only span
    /// measure a hit test can use.
    pub columns: u32,
    /// The link attributes, when this child is a terminal link anchor.
    pub link: Option<PaintedLinkAttributes>,
}

/// One row of painted terminal DOM, as the scanner's input.
///
/// The scan's input is DATA, not elements: the DOM read that produces this is
/// the adapter's, so every rule below is testable without a browser.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaintedRow {
    /// `ROW_HAS_LINKS_ATTR` is set. Absent means there is nothing here to walk,
    /// and a full scan skips the row on this one flag rather than a subtree
    /// query — which is what keeps a scan of held history cheap.
    pub has_links: bool,
    /// The row's painted children in document order.
    pub children: Vec<PaintedChild>,
}
