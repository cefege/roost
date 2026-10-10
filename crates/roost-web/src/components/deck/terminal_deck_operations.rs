//! The deck's commands: select, close, focus, reorder, tile, divider drags,
//! arrange and spotlight. Each one dispatches a `DeckIntent` against the folder
//! the deck painted this frame; the core commits the layout and asks for the
//! route. Built per render by `terminal_deck`, whose strips, bars and dividers
//! call it. Ports `apps/web/src/components/deck/terminal-deck-operations.ts`
//! (its spawn flows are `terminal_deck_spawn`).

use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::{DeckFolder, DeckIntent, DeckSpawn, DeckTab};
use roost_client_core::store::layout::{ArrangeKind, PaneView};
use roost_client_core::store::selectors::session_by_id;
use roost_protocol::layout::document::LayoutDirection;

use super::deck_dom;
use super::terminal_deck_spawn::{spawn_anchor, start_deck_agent, start_deck_spawn};
use crate::motion::drop_zones::{
    DropZone, PaneBox, Rect, SplitDir, tile_target_for, zone_rect, zone_to_split,
};
use crate::motion::resize_drag::ResizeDrag;
use crate::pump::Pump;
use crate::session_actions::{close_labels_for, close_labels_for_agent};

/// The drop-zone highlight a tab drag paints over its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropOverlay {
    /// The highlighted region, deck-local.
    pub rect: Rect,
    /// Its zone.
    pub zone: DropZone,
}

/// What an operation needs from this frame.
#[derive(Clone)]
pub struct DeckOperations {
    pub(super) pump: Pump,
    pub(super) navigate: EventHandler<String>,
    pub(super) folder: Option<DeckFolder>,
    pub(super) focused_pane_id: Option<String>,
    pub(super) compact: bool,
    pub(super) active_session_id: Option<String>,
    pub(super) followed_session_id: Option<String>,
    pub(super) panes: Rc<Vec<PaneView>>,
    pub(super) strip_height: f64,
    pub(super) drag_ratios: Signal<BTreeMap<String, f64>>,
    pub(super) drop_overlay: Signal<Option<DropOverlay>>,
    pub(super) deck_element: Signal<Option<Rc<MountedData>>>,
    pub(super) resize: ResizeDrag,
}

impl std::fmt::Debug for DeckOperations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeckOperations")
            .field("folder", &self.folder)
            .field("compact", &self.compact)
            .finish_non_exhaustive()
    }
}

impl DeckOperations {
    fn deck(&self, intent: DeckIntent) {
        self.pump.dispatch(ClientEvent::Deck(intent));
    }

    /// A tab click, a bar pick or a swipe landing.
    pub fn select(&self, tab_id: String) {
        match self.folder.clone() {
            Some(folder) => self.deck(DeckIntent::SelectTab {
                folder,
                session_id: tab_id,
                compact: self.compact,
            }),
            None => self.navigate.call(
                DeckTab::parse(&tab_id).map_or_else(|| format!("/s/{tab_id}"), |tab| tab.path()),
            ),
        }
    }

    /// A tab's ✕: hide it now, kill it after the undo window.
    pub fn close(&self, tab_id: String) {
        let labels = {
            let core = self.pump.core();
            let core = core.borrow();
            let store = core.store();
            match DeckTab::parse(&tab_id) {
                Some(DeckTab::Agent(conversation_id)) => store
                    .agent_chat
                    .conversations
                    .get(&conversation_id)
                    .map(|conversation| close_labels_for_agent(store, conversation)),
                Some(DeckTab::Terminal(session_id)) => session_by_id(store, session_id.as_str())
                    .map(|session| close_labels_for(store, session)),
                None => None,
            }
        };
        let Some(labels) = labels else {
            return;
        };
        self.deck(DeckIntent::CloseTab {
            folder: self.folder.clone(),
            session_id: tab_id,
            active_session_id: self.active_session_id.clone(),
            labels,
        });
    }

    /// A pane body click: give it the keyboard.
    pub fn focus_pane(&self, pane_id: String) {
        if let Some(folder) = self.folder.clone() {
            self.deck(DeckIntent::FocusPane {
                folder,
                pane_id,
                compact: self.compact,
            });
        }
    }

    /// A tab dragged to a new place in its strip.
    pub fn reorder(&self, pane_id: String, ordered_ids: Vec<String>) {
        if let Some(folder) = self.folder.clone() {
            self.deck(DeckIntent::ReorderTabs {
                folder,
                pane_id,
                ordered_ids,
            });
        }
    }

    /// An arrange preset; every visible pane settles its size claim once.
    pub fn arrange(&self, kind: ArrangeKind) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        self.deck(DeckIntent::Arrange {
            folder,
            kind,
            active_session_id: self.active_session_id.clone(),
        });
        self.resize.pulse_arrange();
    }

    /// Float the focused pane's tab, or put the floated one back.
    pub fn spotlight(&self) {
        if let Some(folder) = self.folder.clone() {
            self.deck(DeckIntent::ToggleSpotlight { folder });
        }
    }

    /// A divider mid-drag paints at its transient ratio.
    pub fn divider_drag(&self, split_id: String, ratio: f64) {
        let mut ratios = self.drag_ratios;
        ratios.write().insert(split_id, ratio);
    }

    /// A divider released: commit the ratio, drop the transient one.
    pub fn divider_commit(&self, split_id: String, ratio: f64) {
        if let Some(folder) = self.folder.clone() {
            self.deck(DeckIntent::SetRatio {
                folder,
                split_id: split_id.clone(),
                ratio,
            });
        }
        let mut ratios = self.drag_ratios;
        ratios.write().remove(&split_id);
    }

    fn tile_target(
        &self,
        origin_pane_id: &str,
        client_x: f64,
        client_y: f64,
    ) -> Option<crate::motion::drop_zones::TileTarget> {
        let origin = self
            .deck_element
            .peek()
            .as_deref()
            .and_then(deck_dom::client_box)
            .unwrap_or_default();
        let boxes: Vec<PaneBox> = self
            .panes
            .iter()
            .map(|pane| PaneBox {
                pane_id: pane.pane_id.clone(),
                rect: Rect {
                    x: pane.rect.x,
                    y: pane.rect.y,
                    w: pane.rect.w,
                    h: pane.rect.h,
                },
            })
            .collect();
        tile_target_for(
            &boxes,
            origin_pane_id,
            client_x - origin.left,
            client_y - origin.top,
            self.strip_height,
        )
    }

    /// A tab drag moved: highlight where it would land.
    pub fn tab_drag_move(&self, origin_pane_id: &str, client_x: f64, client_y: f64) {
        let next = self
            .tile_target(origin_pane_id, client_x, client_y)
            .map(|target| DropOverlay {
                rect: zone_rect(target.rect, target.zone),
                zone: target.zone,
            });
        let mut overlay = self.drop_overlay;
        if *overlay.peek() != next {
            overlay.set(next);
        }
    }

    /// A tab dropped outside its strip: split or merge into the pane under it.
    /// `false` leaves the drop to the strip (a reorder, or off every pane).
    pub fn tab_tile_drop(
        &self,
        tab_id: String,
        origin_pane_id: &str,
        client_x: f64,
        client_y: f64,
    ) -> bool {
        self.tab_drag_end();
        let Some(target) = self.tile_target(origin_pane_id, client_x, client_y) else {
            return false;
        };
        if target.zone == DropZone::Reorder {
            return false;
        }
        let Some(folder) = self.folder.clone() else {
            return false;
        };
        tracing::info!(target: "deck", tab_id, pane_id = target.pane_id, zone = ?target.zone, "tab tiled");
        match zone_to_split(target.zone) {
            Some(placement) => self.deck(DeckIntent::SplitPane {
                folder,
                pane_id: target.pane_id,
                direction: layout_direction(placement.dir),
                tab_id,
                insert_first: placement.insert_first,
            }),
            None => self.deck(DeckIntent::MoveTab {
                folder,
                tab_id,
                to_pane_id: target.pane_id,
            }),
        }
        true
    }

    /// A tab drag ended: no highlight.
    pub fn tab_drag_end(&self) {
        let mut overlay = self.drop_overlay;
        if overlay.peek().is_some() {
            overlay.set(None);
        }
    }

    /// A new terminal beside the pane's selected one, landing in that pane.
    pub fn new_tab(&self, pane_id: String) {
        self.spawn_from(DeckSpawn::NewTab { pane_id });
    }
    /// Create a new agent conversation beside the pane's selected terminal.
    pub fn new_agent(&self, pane_id: String) {
        let spawn = DeckSpawn::NewTab {
            pane_id: pane_id.clone(),
        };
        let anchor = {
            let core = self.pump.core();
            let core = core.borrow();
            spawn_anchor(
                core.store(),
                &self.panes,
                &pane_id,
                self.followed_session_id.as_deref(),
            )
        };
        if let Some(anchor) = anchor {
            start_deck_agent(self.pump.clone(), spawn, anchor, self.compact);
        }
    }

    /// Split the focused pane with a fresh terminal after it.
    pub fn split(&self, direction: LayoutDirection) {
        let Some(pane_id) = self.focused_pane_id.clone() else {
            return;
        };
        self.spawn_from(DeckSpawn::Split { pane_id, direction });
    }

    fn spawn_from(&self, spawn: DeckSpawn) {
        let anchor = {
            let core = self.pump.core();
            let core = core.borrow();
            spawn_anchor(
                core.store(),
                &self.panes,
                spawn.pane_id(),
                self.followed_session_id.as_deref(),
            )
        };
        let Some(anchor) = anchor else {
            tracing::debug!(target: "deck", kind = spawn.kind_name(), "deck spawn has no anchor terminal");
            return;
        };
        start_deck_spawn(self.pump.clone(), spawn, anchor, self.compact);
    }

    /// A pointer went down on the deck: focus the pane under it; a middle
    /// click on a desktop pane (not on a link) floats it.
    pub fn deck_pointer_down(&self, event: &PointerEvent) {
        let target = deck_dom::deck_pointer_target(event);
        if target.in_strip {
            return;
        }
        let Some(pane_id) = target.pane_id else {
            return;
        };
        self.focus_pane(pane_id);
        let middle =
            event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Auxiliary);
        if middle && !self.compact && !target.in_link {
            event.prevent_default();
            self.spotlight();
        }
    }

    /// Focus arrived inside a phone chat input: its pane takes the keyboard.
    pub fn deck_focus_in(&self, event: &FocusEvent) {
        if let Some(pane_id) = deck_dom::chat_input_focus_pane(event)
            && self.focused_pane_id.as_deref() != Some(pane_id.as_str())
        {
            self.focus_pane(pane_id);
        }
    }
}

/// The layout axis a drop split lays its panes along.
pub fn layout_direction(dir: SplitDir) -> LayoutDirection {
    match dir {
        SplitDir::Row => LayoutDirection::Row,
        SplitDir::Col => LayoutDirection::Col,
    }
}
