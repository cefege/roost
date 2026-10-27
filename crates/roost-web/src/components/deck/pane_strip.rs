//! One pane's terminal tab strip: tabs (drag to reorder or tile), the selected
//! agent's actions, the new terminal button, the carrier chip and the overflow
//! list. `TerminalDeck` supplies layout callbacks; this owns strip gestures.
//! Ports `apps/web/src/components/deck/PaneStrip.tsx` and
//! `paneTabRailScroll.ts`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;

use super::deck_dom::{self, ClientBox, SizeWatch, Timeout};
use super::pane_strip_double_press::DoublePress;
use super::pane_strip_gesture::{StripDragOutlets, StripGesture};
use super::pane_tab::PaneTab;
use super::pane_tab_hover_card::PaneTabHoverCard;
use super::pane_tab_list::{PaneTabList, TAB_LIST_MENU_ID};
use super::pane_tab_new_menu::PaneTabNewMenu;
use crate::components::agent_chat::AgentChatHeader;
use crate::components::deck::terminal_deck::AgentInspectorRequest;
use crate::components::layout::window_size::use_is_compact;
use crate::components::terminal::terminal_transport_indicator::TerminalTransportIndicator;
use crate::components::ui::{
    DropdownMenu, DropdownMenuContent, DropdownMenuTrigger, Icon, IconType, Tabs, TabsList,
    TabsVariant, Toolbar,
};
use crate::pump::use_store;
/// How long a pointer rests on a tab before its hover card opens, ms.
const HOVER_DWELL_MS: u32 = 450;
/// How long a closing tab animates out before the close lands, ms.
const CLOSE_ANIMATION_MS: u32 = 220;

/// One strip.
#[component]
#[allow(clippy::too_many_arguments)]
pub fn PaneStrip(
    pane_id: String,
    tab_ids: Vec<String>,
    selected_tab: String,
    focused: bool,
    on_select: EventHandler<String>,
    on_close: EventHandler<String>,
    on_reorder: EventHandler<Vec<String>>,
    on_new_tab: EventHandler<()>,
    #[props(default)] on_new_agent: Option<EventHandler<()>>,
    on_tab_drag_move: Option<EventHandler<(f64, f64)>>,
    on_tab_tile_drop: Option<Callback<(String, f64, f64), bool>>,
    on_tab_drag_end: Option<EventHandler<()>>,
) -> Element {
    let compact = use_is_compact();
    let gesture = StripGesture::use_strip_gesture();
    let closing = use_signal(BTreeSet::<String>::new);
    let mut list_open = use_signal(|| false);
    let mut hover = use_signal(|| None::<(String, ClientBox)>);
    let overflowing = use_signal(|| false);
    let mut filler_press = use_signal(DoublePress::default);
    let timers: Rc<RefCell<Vec<Timeout>>> = use_hook(Rc::default);
    let hover_timer: Rc<RefCell<Option<Timeout>>> = use_hook(Rc::default);
    let rail_watch = use_rail_scroll(gesture, pane_id.clone(), overflowing);
    let hover_card_available = !compact && !deck_dom::is_touch_device();
    let pump = use_store();
    let (selected_agent, online) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let agent = selected_tab
            .strip_prefix("agent:")
            .and_then(|id| store.agent_chat.conversations.get(id))
            .cloned();
        let online = agent.as_ref().is_some_and(|conversation| {
            store
                .workers
                .get(&conversation.worker_fp)
                .is_some_and(|worker| {
                    roost_client_core::store::navigation::worker_online(
                        worker,
                        store.routable_worker_fps.as_ref(),
                        i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX),
                    )
                })
        });
        (agent, online)
    };
    let inspector_request = try_consume_context::<AgentInspectorRequest>();
    let drag = gesture.drag.read().clone();

    use_effect(use_reactive((&selected_tab, &tab_ids), move |_| {
        if let (Some(rail), Some(watch)) =
            (gesture.rail.peek().as_ref(), rail_watch.borrow().as_ref())
        {
            watch.1.watch_tabs_only(rail);
            measure_rail(rail, overflowing);
            if gesture.drag.peek().is_none() {
                deck_dom::reveal_active_tab(rail);
            }
        }
    }));

    let clear_hover = {
        let hover_timer = Rc::clone(&hover_timer);
        move || {
            hover_timer.borrow_mut().take();
            if hover.peek().is_some() {
                let mut clearing = hover;
                clearing.set(None);
            }
        }
    };
    let close_tab = {
        let timers = Rc::clone(&timers);
        move |session_id: String| {
            if deck_dom::reduced_motion() || closing.peek().contains(&session_id) {
                on_close.call(session_id);
                return;
            }
            let mut marking = closing;
            marking.write().insert(session_id.clone());
            let finish = session_id.clone();
            if let Some(timer) = Timeout::after(CLOSE_ANIMATION_MS, move || {
                on_close.call(finish.clone());
                let mut unmarking = closing;
                unmarking.write().remove(&finish);
            }) {
                timers.borrow_mut().push(timer);
            } else {
                on_close.call(session_id);
            }
        }
    };
    let outlets = StripDragOutlets {
        tab_ids: tab_ids.clone(),
        on_reorder,
        on_drag_move: on_tab_drag_move,
        on_tile_drop: on_tab_tile_drop,
        on_drag_end: on_tab_drag_end,
    };
    let hovered = hover.read().clone();
    let clear_hover_on_select = clear_hover.clone();

    rsx! {
        div {
            class: "relative isolate flex min-w-0 h-[var(--workbench-tab-strip-height)] min-h-[var(--workbench-tab-strip-height)] items-center overflow-hidden bg-card pe-[var(--workbench-editor-actions-reserve)]",
            "data-testid": "pane-strip-{pane_id}",
            "data-pane-strip": "{pane_id}",
            "data-focused": if focused { "true" } else { "false" },
            "data-dragging": if drag.is_some() { "true" } else { "false" },
            Tabs {
                class: "contents",
                value: selected_tab.clone(),
                on_value_change: move |session_id| {
                    clear_hover_on_select();
                    on_select.call(session_id);
                },
                div {
                    class: "group/strip flex min-w-0 h-full flex-[0_1_auto] items-center overflow-x-auto overflow-y-hidden scrollbar-none data-[dragging=true]:cursor-grabbing data-[dragging=true]:select-none",
                    "data-tab-bar": "true",
                    "data-focused": if focused { "true" } else { "false" },
                    "data-dragging": if drag.is_some() { "true" } else { "false" },
                    "data-packed": if overflowing() { "true" } else { "false" },
                    onmounted: move |event: MountedEvent| {
                        let mut rail = gesture.rail;
                        rail.set(Some(event.data()));
                    },
                    TabsList {
                        variant: TabsVariant::Line,
                        class: "h-full min-w-0 flex-[0_1_auto] items-center justify-start overflow-visible rounded-none bg-transparent p-0",
                        aria_label: "Terminal tabs",
                        for (index, session_id) in tab_ids.iter().cloned().enumerate() {
                            PaneTab {
                                key: "{session_id}",
                                session_id: session_id.clone(),
                                active: session_id == selected_tab,
                                dragging: drag.as_ref().is_some_and(|active| active.id == session_id),
                                closing: closing.read().contains(&session_id),
                                style: drag.as_ref().map(|active| active.tab_style(index).css()).unwrap_or_default(),
                                hover_card_available,
                                on_pointer_down: {
                                    let outlets = outlets.clone();
                                    let clear_hover = clear_hover.clone();
                                    let session_id = session_id.clone();
                                    move |event: PointerEvent| {
                                        if event.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                                            return;
                                        }
                                        clear_hover();
                                        let point = event.client_coordinates();
                                        gesture.press(session_id.clone(), index, (point.x, point.y), outlets.clone());
                                    }
                                },
                                on_hover_start: {
                                    let hover_timer = Rc::clone(&hover_timer);
                                    let session_id = session_id.clone();
                                    move |anchor: ClientBox| {
                                        if !hover_card_available || gesture.drag.peek().is_some() || *list_open.peek() {
                                            return;
                                        }
                                        let session_id = session_id.clone();
                                        *hover_timer.borrow_mut() = Timeout::after(HOVER_DWELL_MS, move || {
                                            hover.set(Some((session_id, anchor)));
                                        });
                                    }
                                },
                                on_hover_end: {
                                    let clear_hover = clear_hover.clone();
                                    move |()| clear_hover()
                                },
                                on_close: {
                                    let close_tab = close_tab.clone();
                                    let session_id = session_id.clone();
                                    move |event: MouseEvent| {
                                        event.stop_propagation();
                                        event.prevent_default();
                                        close_tab(session_id.clone());
                                    }
                                },
                            }
                        }
                    }
                }
            }
            if let Some(conversation) = selected_agent {
                {
                    let conversation_id = conversation.id.clone();
                    rsx! {
                        AgentChatHeader {
                            conversation,
                            online: Some(online),
                            pump: pump.clone(),
                            on_inspector: move |_| {
                                if let Some(mut request) = inspector_request {
                                    request.0.set(Some(conversation_id.clone()));
                                }
                            },
                        }
                    }
                }
            }
            PaneTabNewMenu {
                on_new_terminal: move |_| on_new_tab.call(()),
                on_new_agent: move |_| {
                    if let Some(on_new_agent) = on_new_agent {
                        on_new_agent.call(());
                    }
                },
            }
            div {
                class: "flex-1 min-w-0 self-stretch",
                "data-testid": "tab-filler",
                title: "Double-click to open a new terminal in this folder",
                onpointerdown: move |event: PointerEvent| {
                    let Some(native) = event.try_as_web_event() else {
                        return;
                    };
                    if native.button() != 0 {
                        return;
                    }
                    let paired = filler_press
                        .with_mut(|press| press.press(native.time_stamp()));
                    if paired {
                        on_new_tab.call(());
                    }
                },
            }
            if !matches!(roost_client_core::deck::tab::DeckTab::parse(&selected_tab), Some(roost_client_core::deck::tab::DeckTab::Agent(_))) {
                TerminalTransportIndicator { session_id: selected_tab.clone() }
            }
            if overflowing() {
                Toolbar { class: "shrink-0", aria_label: "Terminal actions",
                    DropdownMenu {
                        open: list_open(),
                        on_open_change: {
                            let clear_hover = clear_hover.clone();
                            move |open: bool| {
                                if open {
                                    clear_hover();
                                }
                                list_open.set(open);
                            }
                        },
                        test_id: "tab-list-popup",
                        trigger_id: "tab-overflow-{pane_id}",
                        menu_id: TAB_LIST_MENU_ID,
                        DropdownMenuTrigger {
                            class: "shrink-0 size-[var(--workbench-tab-action-size)] p-0",
                            test_id: "tab-overflow",
                            aria_label: "All terminals in this pane",
                            Icon { icon: IconType::ChevronDown, class: "size-4" }
                        }
                        DropdownMenuContent { class: "w-auto min-w-[var(--workbench-tab-menu-min-width)] max-w-96 max-h-96 overflow-y-auto",
                            PaneTabList {
                                tab_ids: tab_ids.clone(),
                                selected_tab: selected_tab.clone(),
                                on_select: move |session_id| on_select.call(session_id),
                                on_reveal_selected: move |()| {
                                    if let Some(rail) = gesture.rail.peek().as_ref()
                                        && gesture.drag.peek().is_none()
                                    {
                                        deck_dom::reveal_active_tab(rail);
                                    }
                                },
                                on_close: move |()| list_open.set(false),
                            }
                        }
                    }
                }
            }
            if let Some((session_id, anchor)) = hovered.filter(|(id, _)| tab_ids.contains(id)) {
                PaneTabHoverCard { session_id, anchor }
            }
        }
    }
}

type RailWatch = Rc<RefCell<Option<(SizeWatch, SizeWatch)>>>;

/// The rail's overflow flag and the reveal of the selected tab: one observer
/// on the rail (measure and reveal) and one on its tabs (measure).
fn use_rail_scroll(gesture: StripGesture, pane_id: String, overflowing: Signal<bool>) -> RailWatch {
    let watch: RailWatch = use_hook(Rc::default);
    let installed = Rc::clone(&watch);
    use_effect(move || {
        let Some(rail) = gesture.rail.read().clone() else {
            return;
        };
        if installed.borrow().is_some() {
            return;
        }
        let rail_for_rail = Rc::clone(&rail);
        let on_rail = SizeWatch::new(move || {
            measure_rail(&rail_for_rail, overflowing);
            if gesture.drag.peek().is_none() {
                deck_dom::reveal_active_tab(&rail_for_rail);
            }
        });
        let rail_for_tabs = Rc::clone(&rail);
        let on_tabs = SizeWatch::new(move || measure_rail(&rail_for_tabs, overflowing));
        if let (Some(on_rail), Some(on_tabs)) = (on_rail, on_tabs) {
            on_rail.watch(&rail);
            on_tabs.watch_tabs_only(&rail);
            *installed.borrow_mut() = Some((on_rail, on_tabs));
        }
        measure_rail(&rail, overflowing);
        tracing::debug!(target: "deck", pane_id = %pane_id, "tab rail observed");
    });
    watch
}

fn measure_rail(rail: &MountedData, mut overflowing: Signal<bool>) {
    let now = deck_dom::rail_overflowing(rail);
    if *overflowing.peek() != now {
        tracing::debug!(target: "deck", overflowing = now, "paneTabs.rail_overflow");
        overflowing.set(now);
    }
}
