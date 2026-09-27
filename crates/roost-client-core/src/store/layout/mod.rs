//! The pane arrangement for ONE folder bucket, and the portable document it
//! travels as. Ported from `apps/web/src/store/paneLayout*.ts`.
//!
//! TWO VOCABULARIES, AND THE BOUNDARY IS THE POINT. `tree` is the RUNTIME
//! arrangement -- runtime pane ids, tab order, focus -- and it exists in no
//! shared crate, because it never crosses a wire. `document` is the portable
//! shape from `roost_protocol::layout`, and it is the only one a coordinator,
//! a CLI or a peer browser ever sees. A document carries leaf and slot KEYS
//! minted from tree position, never a runtime pane id, so two browsers can hold
//! the same arrangement without sharing an id namespace.
//!
//! `record` holds one tree per folder and owns the bounds a persisted tree is
//! read back under. The persistence debounce is the host's, because the core
//! has no timer.

pub mod document;
pub mod geometry;
pub mod presets;
pub mod record;
pub mod tree;
pub mod tree_edit;

pub use document::{
    AppliedLayout, DegradedLayoutDocument, LayoutDocumentError, apply_layout_document,
    degrade_layout_document_to_live_sessions, export_layout_document,
    validate_layout_document_import,
};
pub use geometry::{
    DIVIDER_PX, DividerRect, LayoutRects, PaneRect, PaneView, layout_rects, layout_view,
};
pub use presets::{ArrangeKind, PresetKind, arrange_layout, balance_layout, preset_layout};
pub use record::{
    LAYOUT_STORAGE_KEY, LAYOUT_TREE_MAX_DEPTH, LAYOUT_TREE_MAX_PANES, LayoutRecords,
    LayoutRecordsError,
};
pub use tree::{
    FlatTab, PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, collapse_empties,
    compact_leaf_for_layout, default_layout, find_leaf, find_leaf_of_tab, fix_focus, flat_tabs,
    set_ratio,
};
pub use tree_edit::{
    close_tab, focus_pane, move_tab, reconcile, reorder_tab, select_tab, split_leaf,
};

/// Mints the runtime pane and split ids a materialized tree needs.
///
/// A trait rather than a counter because pane identity must not be derivable
/// from the arrangement: two sessions may hold the same position in two trees,
/// and a deterministic id from that position would make a split look like it
/// already existed. v2 used `crypto.randomUUID()`, so a browser host
/// implements this over WebCrypto and a test over a counted sequence.
pub trait PaneIdSource {
    /// One fresh runtime id, distinct from every id this source has returned.
    fn mint_pane_id(&mut self) -> String;
}
