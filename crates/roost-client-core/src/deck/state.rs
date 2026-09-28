//! The deck's slice of the store: the stored arrangement per folder, the pane
//! id source, the arrangement a pending close restores on undo, and the route
//! the last intent asked for. Mutated only by `deck::intent`; read by the web
//! deck during render. Ports the record half of
//! `apps/web/src/store/paneLayoutStore.ts` as the deck uses it.

use std::collections::BTreeMap;

use crate::platform::KeyValueStore;
use crate::store::layout::{LayoutRecords, PaneIdSource, PaneLayout};

use super::intent::DeckFolder;

/// Mints this page load's runtime pane ids.
///
/// The epoch is the load's clock reading, so an id minted now cannot collide
/// with one a previous load persisted: records outlive the counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckPaneIds {
    epoch_ms: u64,
    minted: u64,
}

impl DeckPaneIds {
    /// A source whose ids are scoped to `epoch_ms`.
    pub fn new(epoch_ms: u64) -> Self {
        Self {
            epoch_ms,
            minted: 0,
        }
    }
}

impl PaneIdSource for DeckPaneIds {
    fn mint_pane_id(&mut self) -> String {
        self.minted += 1;
        format!("pane-{:x}-{}", self.epoch_ms, self.minted)
    }
}

/// The id a render-time resolve gives a folder that has not been seeded yet.
/// The deck seeds it on the next intent, so this id is never persisted.
#[derive(Debug)]
struct UnseededIds;

impl PaneIdSource for UnseededIds {
    fn mint_pane_id(&mut self) -> String {
        "unseeded".to_owned()
    }
}

/// A route an intent asked the host to show, numbered so a host navigates
/// once per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckNavigation {
    /// Monotonic per store.
    pub sequence: u64,
    /// The path, e.g. `/s/<id>`.
    pub path: String,
}

/// The arrangement a close replaced, kept for its undo.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CloseUndo {
    pub(crate) folder_key: String,
    pub(crate) before: PaneLayout,
    pub(crate) was_viewed: bool,
}

/// The deck's state.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckState {
    pub(crate) records: LayoutRecords,
    pub(crate) pane_ids: DeckPaneIds,
    pub(crate) close_undo: BTreeMap<String, CloseUndo>,
    /// The folder the previous observation saw; `None` before the first, so
    /// the first observation is not mistaken for a folder change.
    pub(crate) observed_folder: Option<Option<String>>,
    pub(crate) navigation: Option<DeckNavigation>,
}

impl DeckState {
    /// No stored arrangements; pane ids scoped to `epoch_ms`.
    pub fn new(epoch_ms: u64) -> Self {
        Self {
            records: LayoutRecords::new(),
            pane_ids: DeckPaneIds::new(epoch_ms),
            close_undo: BTreeMap::new(),
            observed_folder: None,
            navigation: None,
        }
    }

    /// The arrangements this browser persisted, or none when the stored record
    /// is absent or refused. A refused record is logged and left in storage
    /// untouched; the next commit overwrites it.
    pub fn restore(storage: &dyn KeyValueStore, epoch_ms: u64) -> Self {
        let mut state = Self::new(epoch_ms);
        if let Err(error) = state.records.restore_from(storage) {
            tracing::warn!(target: "deck", %error, "stored pane layouts refused");
        }
        state
    }

    /// The stored arrangements.
    pub fn records(&self) -> &LayoutRecords {
        &self.records
    }

    /// The records an applied layout document commits into, and the id source
    /// it mints from.
    pub fn layout_state(&mut self) -> (&mut LayoutRecords, &mut dyn PaneIdSource) {
        (&mut self.records, &mut self.pane_ids)
    }

    /// The arrangement a host paints for a folder: the stored one folded against
    /// the live sessions, or a single unseeded pane before the first intent.
    pub fn resolve_layout(&self, folder: &DeckFolder) -> PaneLayout {
        self.records.resolve(
            &folder.folder_key,
            &folder.live_session_ids,
            &mut UnseededIds,
        )
    }

    /// The route the newest intent asked for.
    pub fn navigation(&self) -> Option<&DeckNavigation> {
        self.navigation.as_ref()
    }

    /// The live arrangement for an intent, seeding the folder first so every
    /// commit derives from stable pane ids.
    pub(crate) fn resolve_for_edit(&mut self, folder: &DeckFolder) -> PaneLayout {
        self.records.seed_if_absent(
            &folder.folder_key,
            &folder.live_session_ids,
            &mut self.pane_ids,
        );
        self.records.resolve(
            &folder.folder_key,
            &folder.live_session_ids,
            &mut self.pane_ids,
        )
    }

    /// Replace a folder's arrangement and persist the record.
    pub(crate) fn commit(
        &mut self,
        folder_key: &str,
        layout: PaneLayout,
        storage: &dyn KeyValueStore,
    ) {
        self.records.commit(folder_key, layout);
        if let Err(error) = self.records.persist(storage) {
            tracing::warn!(target: "deck", %error, "pane layouts not persisted");
        }
    }

    /// Ask the host to show `path`.
    pub(crate) fn request_navigation(&mut self, path: String) {
        let sequence = self.navigation.as_ref().map_or(1, |last| last.sequence + 1);
        tracing::debug!(target: "deck", sequence, path = %path, "deck navigation requested");
        self.navigation = Some(DeckNavigation { sequence, path });
    }
}
