//! The controller router's app half, over the live store, overlays and deck.
//!
//! `pad_router` decides what an intent means; this performs it against the same
//! APIs every other surface uses — the shell's overlay flags, `ShellIntent` for
//! the store-level actions, `DeckIntent` for tab and pane selection, the router
//! for navigation, and the page's voice slot for dictation. Nothing here
//! re-implements a decision the router already made.
//!
//! Installed once by `app::install_document_input`, which hands its actions to
//! `pad_dom::dispatch_pad_actions`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::DeckIntent;
use roost_client_core::store::layout::{PaneLayout, all_leaves, find_leaf};
use roost_client_core::store::selectors::session_folder_key;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::build_folder_groups;
use roost_client_core::store::terminal_nav_pad::terminal_nav_pad_open;

use crate::components::deck::terminal_deck_model::deck_folder_for;
use crate::input_nav::pad_folders::FolderLead;
use crate::input_nav::pad_surfaces::{
    PadDictation, PadFolderCycle, PadPaneTarget, PadShellAction, PadSurfaceState, PadSurfaces,
};
use crate::keyboard_shortcuts::ShortcutOverlays;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::route_session::active_session_for_path;
use crate::routes::session_href;
use crate::voice::shell_controls;

/// The live shell the controller router drives.
#[derive(Clone)]
pub struct ShellPadSurfaces {
    pump: Pump,
    overlays: ShortcutOverlays,
    route: Signal<String>,
    compact: bool,
}

impl std::fmt::Debug for ShellPadSurfaces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellPadSurfaces")
            .field("compact", &self.compact)
            .finish_non_exhaustive()
    }
}

impl ShellPadSurfaces {
    /// The shell, reading the store through `pump` on every call so a decision
    /// never rests on a snapshot the router can see past.
    ///
    /// `compact` is the size class the shell was built in: the deck intents need
    /// it, and a pad press happens between renders, so it is captured once
    /// beside the router rather than read through a component hook.
    pub fn new(
        pump: Pump,
        overlays: ShortcutOverlays,
        route: Signal<String>,
        compact: bool,
    ) -> Self {
        Self {
            pump,
            overlays,
            route,
            compact,
        }
    }

    /// The route's open session and the folder it belongs to.
    fn routed_folder(&self) -> Option<(String, roost_client_core::deck::DeckFolder)> {
        let core = self.pump.core();
        let core = core.borrow();
        let store = core.store();
        let path = (self.route.peek()).clone();
        let session = active_session_for_path(store, &BrowserWorkerPaths, &path)?;
        let folder = deck_folder_for(store, &BrowserWorkerPaths, session);
        Some((session.id.as_str().to_owned(), folder))
    }

    /// The arrangement the route's session paints.
    fn routed_layout(&self) -> Option<(String, PaneLayout)> {
        let (session_id, folder) = self.routed_folder()?;
        let core = self.pump.core();
        let core = core.borrow();
        Some((session_id, core.store().deck.resolve_layout(&folder)))
    }
}

impl PadSurfaces for ShellPadSurfaces {
    fn state(&self) -> PadSurfaceState {
        let core = self.pump.core();
        let core = core.borrow();
        let store = core.store();
        let facts = shell_controls::dictation_facts();
        PadSurfaceState {
            keypad_open: terminal_nav_pad_open(store),
            controller_map_open: (self.overlays.controller_map)(),
            palette_open: (self.overlays.palette)(),
            help_open: (self.overlays.help)(),
            sidebar_open: store.ui.sidebar_open,
            dictation: PadDictation {
                dictating: facts.dictating,
                controls_mounted: facts.controls_mounted,
                can_start_without_gesture: facts.can_start_without_gesture,
            },
        }
    }

    fn target_pane(&self, focused_pane_id: Option<&str>) -> Option<PadPaneTarget> {
        let (_, layout) = self.routed_layout()?;
        // Directional travel deliberately does not move `focusedPaneId`, so the
        // pane holding DOM focus wins and mere focus movement never re-navigates.
        let pane_id = focused_pane_id
            .filter(|pane_id| !pane_id.is_empty())
            .unwrap_or(layout.focused_pane_id.as_str());
        let leaf = find_leaf(&layout.root, pane_id)?;
        Some(PadPaneTarget {
            pane_id: leaf.pane_id.clone(),
            tabs: leaf.tabs.clone(),
            selected_tab: leaf.selected_tab.clone(),
            layout_pane_ids: all_leaves(&layout.root)
                .into_iter()
                .map(|leaf| leaf.pane_id.clone())
                .collect(),
        })
    }

    fn folder_cycle(&self) -> Option<PadFolderCycle> {
        let core = self.pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let path = (self.route.peek()).clone();
        let folders = build_folder_groups(store, &BrowserWorkerPaths, now_ms)
            .into_iter()
            .map(|group| FolderLead {
                key: group.key,
                lead_id: group.lead_id,
            })
            .collect();
        Some(PadFolderCycle {
            folders,
            current_folder_key: active_session_for_path(store, &BrowserWorkerPaths, &path)
                .map(|session| session_folder_key(store, &BrowserWorkerPaths, session)),
        })
    }

    fn execute(&mut self, action: PadShellAction) {
        match action {
            PadShellAction::OpenPalette => {
                let mut palette = self.overlays.palette;
                palette.set(true);
                tracing::info!(target: "input_nav", "pad opened the command palette");
            }
            PadShellAction::ClosePalette => {
                let mut palette = self.overlays.palette;
                palette.set(false);
            }
            PadShellAction::OpenControllerMap => {
                let mut map = self.overlays.controller_map;
                map.set(true);
                tracing::info!(target: "input_nav", "pad opened the controller map");
            }
            PadShellAction::CloseControllerMap => {
                let mut map = self.overlays.controller_map;
                map.set(false);
            }
            PadShellAction::ToggleKeypad => {
                self.pump
                    .dispatch(ClientEvent::Shell(ShellIntent::ToggleNavPad));
            }
            PadShellAction::CloseKeypad => {
                self.pump
                    .dispatch(ClientEvent::Shell(ShellIntent::CloseNavPad));
            }
            PadShellAction::CloseSidebar => {
                self.pump
                    .dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
            }
            PadShellAction::ToggleDictation => shell_controls::toggle(),
            PadShellAction::DiscardDictation => shell_controls::discard(),
            PadShellAction::SelectTab { tab } => {
                if let Some(folder) = self.routed_folder().map(|(_, folder)| folder) {
                    self.pump.dispatch(ClientEvent::Deck(DeckIntent::SelectTab {
                        folder,
                        session_id: tab,
                        compact: self.compact,
                    }));
                }
            }
            PadShellAction::FocusPane { pane_id } => {
                if let Some(folder) = self.routed_folder().map(|(_, folder)| folder) {
                    self.pump.dispatch(ClientEvent::Deck(DeckIntent::FocusPane {
                        folder,
                        pane_id,
                        compact: self.compact,
                    }));
                }
            }
            PadShellAction::OpenSession { session_id } => {
                crate::router_state::navigate_path(self.route, session_href(&session_id));
            }
            PadShellAction::Warn { message } => {
                self.pump
                    .dispatch(ClientEvent::Shell(ShellIntent::ShowWarning {
                        message: message.to_owned(),
                    }));
            }
        }
    }
}
