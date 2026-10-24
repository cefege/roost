//! Store fields that trigger a terminal surface repaint without subscribing to every frame.

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PaneStoreView {
    pub pending: bool,
    pub title: String,
    pub offline_sibling: Option<String>,
    pub drawer_open: bool,
    pub nav_pad_open: bool,
}
