//! One stored arrangement per folder bucket, and the bounds a persisted one is
//! read back under.
//!
//! Ported from `apps/web/src/store/paneLayoutStore.ts`, minus the two things a
//! synchronous core cannot own: the per-key reactive signals (a host's job) and
//! the 300ms persist debounce (a timer, and the core has none). What is left is
//! the record, and the record is a value a host holds, not a module-level
//! global -- two clients in one process must not share one.
//!
//! A STORED TREE IS UNTRUSTED ON THE WAY BACK IN. The key/value store is
//! writable by the user and by any script on the origin, so `restore` refuses a
//! tree that is too deep, too wide, out of ratio bounds, or internally
//! inconsistent, and refuses the WHOLE payload rather than the folders that
//! parsed: a half-restored record is a client that paints a different
//! arrangement before and after a reload.

use std::collections::BTreeMap;

use roost_protocol::layout::{LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_NODES};

use crate::platform::KeyValueStore;

use super::PaneIdSource;
use super::tree::{PaneLayout, PaneNode, default_layout, find_leaf};
use super::tree_edit::reconcile;

/// The key every folder's arrangements are persisted under.
pub const LAYOUT_STORAGE_KEY: &str = "roost.paneLayout.v1";

/// How deep a persisted tree may be. The portable document's bound, so a tree
/// read back from storage can always be exported as a document.
pub const LAYOUT_TREE_MAX_DEPTH: usize = LAYOUT_DOCUMENT_MAX_DEPTH;

/// How many panes a persisted tree may hold. The portable document's node bound,
/// for the same reason.
pub const LAYOUT_TREE_MAX_PANES: usize = LAYOUT_DOCUMENT_MAX_NODES;

/// Why a persisted record could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutRecordsError {
    /// The payload is not a record of folder arrangements.
    NotARecord(String),
    /// A folder's stored tree failed an invariant.
    Malformed {
        /// The folder bucket.
        folder_key: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The record could not be encoded.
    Serialization(String),
}

impl std::fmt::Display for LayoutRecordsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARecord(reason) => {
                write!(formatter, "layout record is not a record: {reason}")
            }
            Self::Malformed { folder_key, reason } => {
                write!(
                    formatter,
                    "stored layout for {folder_key} is malformed: {reason}"
                )
            }
            Self::Serialization(reason) => write!(formatter, "layout record: {reason}"),
        }
    }
}

impl std::error::Error for LayoutRecordsError {}

/// The stored arrangements, one per folder bucket.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutRecords {
    folders: BTreeMap<String, PaneLayout>,
}

impl LayoutRecords {
    /// No stored arrangements.
    pub fn new() -> Self {
        Self::default()
    }

    /// The stored arrangement for a folder, if it has one.
    pub fn stored(&self, folder_key: &str) -> Option<&PaneLayout> {
        self.folders.get(folder_key)
    }

    /// How many folders have a stored arrangement.
    pub fn len(&self) -> usize {
        self.folders.len()
    }

    /// Whether nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    /// Persist a stable default the first time a folder becomes active.
    ///
    /// Seeded rather than derived on every read: a derived default mints a new
    /// pane id each time, and a churning pane id is a deck that remounts every
    /// terminal on every render.
    pub fn seed_if_absent(
        &mut self,
        folder_key: &str,
        live_session_ids: &[String],
        ids: &mut dyn PaneIdSource,
    ) {
        if folder_key.is_empty() || self.folders.contains_key(folder_key) {
            return;
        }
        self.commit(folder_key, default_layout(live_session_ids, ids));
    }

    /// The arrangement a host should paint for a folder: the stored one folded
    /// against the live session set, or a derived default when nothing is
    /// stored.
    pub fn resolve(
        &self,
        folder_key: &str,
        live_session_ids: &[String],
        ids: &mut dyn PaneIdSource,
    ) -> PaneLayout {
        let stored = self
            .folders
            .get(folder_key)
            .cloned()
            .unwrap_or_else(|| default_layout(&[], ids));
        reconcile(&stored, live_session_ids)
    }

    /// Replace a folder's stored arrangement.
    pub fn commit(&mut self, folder_key: &str, layout: PaneLayout) {
        let panes = super::tree::all_leaves(&layout.root).len();
        self.folders.insert(folder_key.to_owned(), layout);
        tracing::info!(
            target: "layout",
            folder_key,
            panes,
            "committed pane layout"
        );
    }

    /// Forget one folder's arrangement.
    pub fn remove(&mut self, folder_key: &str) {
        if self.folders.remove(folder_key).is_some() {
            tracing::info!(target: "layout", folder_key, "forgot pane layout");
        }
    }

    /// Forget every arrangement, which is what a logout owes: the ids in them
    /// belong to the account that is leaving.
    pub fn clear(&mut self) {
        let folders = self.folders.len();
        self.folders.clear();
        tracing::info!(target: "layout", folders, "cleared every pane layout");
    }

    /// The record as the host persists it.
    pub fn snapshot(&self) -> Result<String, LayoutRecordsError> {
        serde_json::to_string(&self.folders)
            .map_err(|error| LayoutRecordsError::Serialization(error.to_string()))
    }

    /// Write the record into a key/value store. The host decides when: a commit
    /// fires per focus click and per divider drag, and one write per burst is
    /// the host's debounce to own.
    pub fn persist(&self, store: &dyn KeyValueStore) -> Result<(), LayoutRecordsError> {
        match self.snapshot() {
            Ok(payload) => {
                store.set(LAYOUT_STORAGE_KEY, &payload);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Read the record back from a key/value store. A key that is absent is an
    /// empty record, not a failure: a first run has nothing to restore.
    pub fn restore_from(&mut self, store: &dyn KeyValueStore) -> Result<usize, LayoutRecordsError> {
        match store.get(LAYOUT_STORAGE_KEY) {
            Some(payload) => self.restore(&payload),
            None => Ok(0),
        }
    }

    /// Read a persisted record, all of it or none of it.
    pub fn restore(&mut self, payload: &str) -> Result<usize, LayoutRecordsError> {
        let folders: BTreeMap<String, PaneLayout> =
            serde_json::from_str::<BTreeMap<String, PaneLayout>>(payload)
                .map_err(|error| LayoutRecordsError::NotARecord(error.to_string()))?;
        // Every folder is proved before ANY of them is installed, so one bad
        // folder leaves the record exactly as it was rather than half-restored.
        for (folder_key, layout) in &folders {
            validate_stored(folder_key, layout)?;
        }
        let restored = folders.len();
        self.folders = folders;
        tracing::info!(target: "layout", restored, "restored pane layouts");
        Ok(restored)
    }
}

fn validate_stored(folder_key: &str, layout: &PaneLayout) -> Result<(), LayoutRecordsError> {
    // ITERATIVE, and both bounds counted in one walk: the tree is untrusted, so
    // the depth cap has to be established before any recursion, and the pane
    // cap has to be counted in the same pass or a wide-but-shallow tree gets a
    // walk whose cost is bounded by neither.
    let mut pending: Vec<(&PaneNode, usize)> = vec![(&layout.root, 1)];
    let mut panes = 0usize;
    while let Some((node, depth)) = pending.pop() {
        if depth > LAYOUT_TREE_MAX_DEPTH {
            return Err(malformed(
                folder_key,
                format!("exceeds depth {LAYOUT_TREE_MAX_DEPTH}"),
            ));
        }
        match node {
            PaneNode::Leaf(leaf) => {
                panes += 1;
                if panes > LAYOUT_TREE_MAX_PANES {
                    return Err(malformed(
                        folder_key,
                        format!("exceeds {LAYOUT_TREE_MAX_PANES} panes"),
                    ));
                }
                // A non-empty pane must name a tab it holds. Both ends: the
                // empty case is legal and is the only one, and a pane that
                // selects a tab it does not hold paints a terminal the deck has
                // no rect for.
                if !leaf.tabs.is_empty() && !leaf.tabs.iter().any(|tab| *tab == leaf.selected_tab) {
                    return Err(malformed(
                        folder_key,
                        format!("pane {} selects a tab it does not hold", leaf.pane_id),
                    ));
                }
            }
            PaneNode::Split(split) => {
                if !split.ratio.is_finite()
                    || !(roost_protocol::layout::LAYOUT_RATIO_MIN
                        ..=roost_protocol::layout::LAYOUT_RATIO_MAX)
                        .contains(&split.ratio)
                {
                    return Err(malformed(
                        folder_key,
                        format!("split {} has ratio {} out of bounds", split.id, split.ratio),
                    ));
                }
                pending.push((&split.a, depth + 1));
                pending.push((&split.b, depth + 1));
            }
        }
    }
    if find_leaf(&layout.root, &layout.focused_pane_id).is_none() {
        return Err(malformed(
            folder_key,
            format!("focused pane {} is not in the tree", layout.focused_pane_id),
        ));
    }
    Ok(())
}

fn malformed(folder_key: &str, reason: String) -> LayoutRecordsError {
    LayoutRecordsError::Malformed {
        folder_key: folder_key.to_owned(),
        reason,
    }
}
