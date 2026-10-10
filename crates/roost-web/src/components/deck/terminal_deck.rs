//! The terminal deck: the persistent surface behind every terminal route. It
//! keeps each warm session's `CellTerminal` mounted, keyed by id so a store
//! revision never remounts a renderer, paints the followed folder's panes,
//! strips, dividers, phone bars and spotlight, and follows the routes the core
//! asks for. Mounted by `main_pane::MainPane`; state is `roost_client_core::deck`.
//! Ports `apps/web/src/components/deck/TerminalDeck.tsx`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::{
    DeckIntent, DeckNavigation, TerminalSessionSlot, mounted_session_ids,
};
use roost_client_core::store::Session;
use roost_client_core::store::selectors::session_by_id;
use roost_protocol::wire::agent_chat::ConversationSummary;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::deck_dom::DeckContainer;
use super::deck_swipe::Swipe;
use super::deck_swipe_style::swipe_style_for;
use super::terminal_deck_chords::{DeckChordState, use_deck_chords};
use super::terminal_deck_chrome::{compact_chrome, desktop_chrome};
use super::terminal_deck_geometry::terminal_session_style;
use super::terminal_deck_hooks::{
    DeckObservation, use_deck_measure, use_deck_navigation, use_deck_observation, use_warm_set,
};
use super::terminal_deck_model::{DeckFrame, DeckInputs, deck_frame};
use super::terminal_deck_operations::{DeckOperations, DropOverlay};
use super::terminal_deck_spotlight::TerminalDeckSpotlight;
use super::terminal_deck_swipe::{SwipeFrame, use_deck_swipe};
use super::terminal_deck_swipe_overlay::TerminalDeckSwipeOverlay;
use crate::components::layout::window_size::use_is_compact;
use crate::components::terminal::cell_terminal::CellTerminal;
use crate::motion::resize_drag::use_resize_drag;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// One mounted terminal: its session row and where it paints.
#[derive(Debug, Clone, PartialEq)]
struct MountedTerminal {
    session: Session,
    slot: Option<TerminalSessionSlot>,
    style: String,
}

#[derive(Debug, Clone, PartialEq)]
struct MountedAgent {
    tab_id: String,
    conversation: ConversationSummary,
    slot: Option<TerminalSessionSlot>,
    style: String,
}

type DeckRenderSnapshot = (
    DeckFrame,
    Option<DeckNavigation>,
    BTreeMap<String, Session>,
    BTreeMap<String, ConversationSummary>,
    bool,
);

/// The deck. Props are v2's `TerminalDeckProps`, snake-cased.
#[component]
pub fn TerminalDeck(active_session_id: Option<String>, surface_visible: bool) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let resize = use_resize_drag();
    let measure = use_deck_measure();
    use_context_provider(|| DeckContainer(measure.element));
    let mut retained = use_signal(|| active_session_id.clone());
    use_effect(use_reactive((&active_session_id,), move |(active,)| {
        if active.is_some() && *retained.peek() != active {
            retained.set(active);
        }
    }));
    let swipe = use_signal(|| None::<Swipe>);
    let drag_ratios = use_signal(BTreeMap::<String, f64>::new);
    let drop_overlay = use_signal(|| None::<DropOverlay>);

    let size = (measure.size)();
    let desktop_strip_height = (measure.desktop_strip_height)();
    let retained_id = retained.read().clone();
    let swipe_now = swipe.read().clone();
    let (frame, navigation, sessions, conversations, store_spotlight_active): DeckRenderSnapshot = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let ratios = drag_ratios.read();
        let frame = deck_frame(
            store,
            &BrowserWorkerPaths,
            &DeckInputs {
                active_session_id: active_session_id.as_deref(),
                surface_visible,
                retained_session_id: retained_id.as_deref(),
                compact,
                size,
                desktop_strip_height,
                drag_ratios: &ratios,
                swipe_neighbor_id: swipe_now
                    .as_ref()
                    .and_then(|swipe| swipe.neighbor_id.as_deref()),
            },
        );
        let sessions = frame
            .open_session_ids
            .iter()
            .filter_map(|id| session_by_id(store, id).map(|session| (id.clone(), session.clone())))
            .collect();
        let conversations = frame
            .open_session_ids
            .iter()
            .filter_map(|tab_id| {
                let id = tab_id.strip_prefix("agent:")?;
                store
                    .agent_chat
                    .conversations
                    .get(id)
                    .cloned()
                    .map(|conversation| (tab_id.clone(), conversation))
            })
            .collect();
        (
            frame,
            store.deck.navigation().cloned(),
            sessions,
            conversations,
            store.spotlight.session_id().is_some(),
        )
    };

    let slotted: Vec<String> = frame.slots.keys().cloned().collect();
    let warm = use_warm_set(&frame.open_session_ids, &slotted);
    let mounted_ids = mounted_session_ids(&frame.open_session_ids, &warm.read(), &frame.slots);
    use_deck_observation(
        &pump,
        DeckObservation {
            folder: frame.folder.clone(),
            followed_session_id: frame.followed_session_id.clone(),
            compact,
            visible_pane_count: u32::try_from(frame.view.panes.len()).unwrap_or(u32::MAX),
        },
    );
    use_deck_navigation(navigation);

    let operations = DeckOperations {
        pump: pump.clone(),
        navigate,
        folder: frame.folder.clone(),
        focused_pane_id: frame
            .layout
            .as_ref()
            .map(|layout| layout.focused_pane_id.clone()),
        compact,
        active_session_id: active_session_id.clone(),
        followed_session_id: frame.followed_session_id.clone(),
        panes: Rc::new(frame.view.panes.clone()),
        strip_height: frame.strip_height,
        drag_ratios,
        drop_overlay,
        deck_element: measure.element,
        resize,
    };
    let spotlight_active = frame.spotlight_pane_id.is_some();
    let terminals: Vec<MountedTerminal> = mounted_ids
        .iter()
        .filter_map(|id| {
            let session = sessions.get(id)?.clone();
            let slot = frame.slots.get(id).cloned();
            let style = terminal_session_style(
                slot.as_ref(),
                frame.park_sizes.get(id).copied(),
                size,
                frame.strip_height,
                compact,
            )
            .merged(&swipe_style_for(swipe_now.as_ref(), id, size.w))
            .css();
            Some(MountedTerminal {
                session,
                slot,
                style,
            })
        })
        .collect();
    let agents: Vec<MountedAgent> = mounted_ids
        .iter()
        .filter_map(|tab_id| {
            let conversation = conversations.get(tab_id)?.clone();
            let slot = frame.slots.get(tab_id).cloned();
            let style = terminal_session_style(
                slot.as_ref(),
                frame.park_sizes.get(tab_id).copied(),
                size,
                frame.strip_height,
                compact,
            )
            .merged(&swipe_style_for(swipe_now.as_ref(), tab_id, size.w))
            .css();
            Some(MountedAgent {
                tab_id: tab_id.clone(),
                conversation,
                slot,
                style,
            })
        })
        .collect();
    let multi_pane = if frame.view.panes.len() > 1 {
        "true"
    } else {
        "false"
    };
    let deck_style = format!(
        "flex: 1; position: relative; overflow: hidden; background: var(--term-bg); touch-action: {}; transform: translate3d(0, calc(0px - var(--term-chat-growth, 0px)), 0);",
        if compact { "pan-y" } else { "auto" },
    );
    use_deck_chords(DeckChordState {
        operations: operations.clone(),
        surface_visible,
        spotlight_active: store_spotlight_active,
    });
    use_deck_swipe(
        swipe,
        measure.element,
        SwipeFrame {
            compact,
            width: size.w,
            mobile_tabs: frame.mobile_tabs.clone(),
            active_session_id: active_session_id.clone(),
            operations: operations.clone(),
        },
        frame
            .folder
            .as_ref()
            .map(|folder| folder.folder_key.clone()),
        frame.followed_session_id.clone(),
    );
    let pointer_ops = operations.clone();
    let focus_ops = operations.clone();
    let attach = measure.clone();
    let dismiss_pump = pump.clone();

    rsx! {
        div {
            class: "workbench-terminal-deck",
            "data-testid": "terminal-deck",
            "data-multi-pane": multi_pane,
            "data-resizing": resize.is_dragging().then_some("true"),
            style: deck_style,
            onmounted: move |event: MountedEvent| attach.attach(event.data()),
            onpointerdown: move |event: PointerEvent| pointer_ops.deck_pointer_down(&event),
            onfocusin: move |event: FocusEvent| focus_ops.deck_focus_in(&event),
            if frame.open_session_ids.is_empty() {
                div { class: "workbench-terminal-deck__empty", "No session selected." }
            }
            for terminal in terminals {
                TerminalSlot {
                    key: "{terminal.session.id.as_str()}",
                    terminal,
                    surface_visible,
                    spotlight_active,
                }
            }
            for agent in agents {
                AgentSlot {
                    key: "{agent.tab_id}",
                    agent,
                    surface_visible,
                    spotlight_active,
                }
            }
            if compact {
                {compact_chrome(&frame, &operations, swipe_now.as_ref(), active_session_id.as_deref().unwrap_or_default(), size.w)}
            }
            TerminalDeckSwipeOverlay {
                compact,
                swipe: swipe_now.clone(),
                pane_rect: frame.view.panes.first().map(|pane| pane.rect),
                deck_width: size.w,
                strip_height: frame.strip_height,
                folder_label: frame.new_terminal_folder.clone(),
            }
            if !compact {
                {desktop_chrome(&frame, &operations, *drop_overlay.read())}
            }
            TerminalDeckSpotlight {
                rect: frame.spotlight_rect,
                on_dismiss: move |()| dismiss_pump.dispatch(ClientEvent::Deck(DeckIntent::ClearSpotlight)),
            }
        }
    }
}

/// One keyed slot around a `CellTerminal`.
#[component]
fn TerminalSlot(
    terminal: MountedTerminal,
    surface_visible: bool,
    spotlight_active: bool,
) -> Element {
    let session_id = terminal.session.id.as_str().to_owned();
    let slot = terminal.slot.as_ref();
    let pane_id = slot.map_or_else(String::new, |slot| slot.pane_id.clone());
    let focused = slot.is_some_and(|slot| slot.focused);
    let spotlit = slot.is_some_and(|slot| slot.spotlit);
    rsx! {
        div {
            class: "workbench-terminal-slot",
            "data-testid": "terminal-slot-{session_id}",
            "data-pane-slot": "true",
            "data-pane": "true",
            "data-pane-id": pane_id,
            "data-focused": if focused { "true" } else { "false" },
            "data-spotlit": spotlit.then_some("true"),
            style: terminal.style.clone(),
            CellTerminal {
                session: terminal.session.clone(),
                in_layout: slot.is_some(),
                focused,
                spotlit,
                surface_visible,
                surface_active: !spotlight_active || spotlit,
            }
        }
    }
}

#[component]
fn AgentSlot(agent: MountedAgent, surface_visible: bool, spotlight_active: bool) -> Element {
    let slot = agent.slot.as_ref();
    let pane_id = slot.map_or_else(String::new, |slot| slot.pane_id.clone());
    let focused = slot.is_some_and(|slot| slot.focused);
    let spotlit = slot.is_some_and(|slot| slot.spotlit);
    let hidden =
        !surface_visible || (!spotlight_active && !focused) || (spotlight_active && !spotlit);
    rsx! {
        div {
            class: "workbench-terminal-slot",
            "data-testid": "agent-slot-{agent.conversation.id}",
            "data-pane-slot": "true",
            "data-pane": "true",
            "data-pane-id": pane_id,
            "data-focused": if focused { "true" } else { "false" },
            "data-spotlit": spotlit.then_some("true"),
            style: agent.style,
            aria_hidden: hidden,
            crate::components::agent_chat::AgentChatSurface {
                conversation_id: agent.conversation.id.clone(),
            }
        }
    }
}
