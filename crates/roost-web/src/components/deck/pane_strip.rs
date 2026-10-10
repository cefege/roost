//! One pane's terminal tab strip: the tabs (drag to reorder or tile), the new
//! terminal button, the double-click filler, the carrier chip, the overflow
//! list and the dwell hover card. `TerminalDeck` supplies the layout
//! callbacks; this owns the strip's transient state. Ports
//! `apps/web/src/components/deck/PaneStrip.tsx` and `paneTabRailScroll.ts`.

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
use super::pane_tab_list::PaneTabList;
use super::pane_tab_new_menu::PaneTabNewMenu;
use crate::components::context_menu::AnchoredMenuPos;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{IconButton, IconButtonSize};
use crate::components::terminal::terminal_transport_indicator::TerminalTransportIndicator;
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
    let pump = use_store();
    let agent_enabled = pump
        .core()
        .borrow()
        .store()
        .coord_identity
        .as_ref()
        .is_some_and(|identity| identity.builtin_agent_enabled);
    let compact = use_is_compact();
    let gesture = StripGesture::use_strip_gesture();
    let closing = use_signal(BTreeSet::<String>::new);
    let mut list_open = use_signal(|| None::<AnchoredMenuPos>);
    let mut hover = use_signal(|| None::<(String, ClientBox)>);
    let overflowing = use_signal(|| false);
    let mut overflow_button = use_signal(|| None::<Rc<MountedData>>);
    let mut filler_press = use_signal(DoublePress::default);
    let timers: Rc<RefCell<Vec<Timeout>>> = use_hook(Rc::default);
    let hover_timer: Rc<RefCell<Option<Timeout>>> = use_hook(Rc::default);
    let rail_watch = use_rail_scroll(gesture, pane_id.clone(), overflowing);
    let hover_card_available = !compact && !deck_dom::is_touch_device();
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
    let open_position = *list_open.read();

    rsx! {
        div {
            class: "workbench-pane-tab-strip",
            "data-testid": "pane-strip-{pane_id}",
            "data-pane-strip": "{pane_id}",
            "data-focused": if focused { "true" } else { "false" },
            "data-dragging": if drag.is_some() { "true" } else { "false" },
            div {
                class: "df-tab-bar workbench-pane-tab-strip__tabs",
                "data-focused": if focused { "true" } else { "false" },
                "data-dragging": if drag.is_some() { "true" } else { "false" },
                "data-packed": if overflowing() { "true" } else { "false" },
                onmounted: move |event: MountedEvent| {
                    let mut rail = gesture.rail;
                    rail.set(Some(event.data()));
                },
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
                        on_select: {
                            let clear_hover = clear_hover.clone();
                            let session_id = session_id.clone();
                            move |()| {
                                clear_hover();
                                on_select.call(session_id.clone());
                            }
                        },
                        on_hover_start: {
                            let hover_timer = Rc::clone(&hover_timer);
                            let session_id = session_id.clone();
                            move |anchor: ClientBox| {
                                if !hover_card_available || gesture.drag.peek().is_some() || list_open.peek().is_some() {
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
            PaneTabNewMenu {
                pane_id: pane_id.clone(),
                agent_enabled,
                on_new_terminal: move |_| on_new_tab.call(()),
                on_new_agent: move |_| {
                    if let Some(on_new_agent) = on_new_agent {
                        on_new_agent.call(());
                    }
                },
            }
            div {
                class: "df-tab-filler workbench-pane-tab-strip__filler",
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
            div { class: "workbench-pane-tab-strip__actions", role: "toolbar", "aria-label": "Terminal actions",
                if overflowing() {
                    IconButton {
                        icon: "keyboard_arrow_down",
                        label: "All terminals in this pane",
                        size: IconButtonSize::IconSm,
                        class: "df-tab-overflow workbench-pane-tab-control",
                        "data-testid": "tab-overflow",
                        id: "tab-overflow-{pane_id}",
                        menu_popup: "menu",
                        controls_id: "tab-list-popup",
                        expanded: open_position.is_some(),
                        title: "All terminals",
                        onmounted: move |event: MountedEvent| overflow_button.set(Some(event.data())),
                        onclick: {
                            let clear_hover = clear_hover.clone();
                            move |_| {
                                if list_open.peek().is_some() {
                                    list_open.set(None);
                                    return;
                                }
                                let Some(anchor) = overflow_button.peek().as_deref().and_then(deck_dom::client_box) else {
                                    return;
                                };
                                clear_hover();
                                list_open.set(Some(crate::components::context_menu::anchored_menu_pos(
                                    anchor.left + anchor.width,
                                    anchor.bottom(),
                                    deck_dom::viewport_width(),
                                )));
                            }
                        },
                    }
                }
            }
            if let Some(position) = open_position {
                PaneTabList {
                    position,
                    tab_ids: tab_ids.clone(),
                    selected_tab: selected_tab.clone(),
                    trigger_id: format!("tab-overflow-{pane_id}"),
                    on_select: move |session_id| on_select.call(session_id),
                    on_reveal_selected: move |()| {
                        if let Some(rail) = gesture.rail.peek().as_ref()
                            && gesture.drag.peek().is_none()
                        {
                            deck_dom::reveal_active_tab(rail);
                        }
                    },
                    on_close: move |()| list_open.set(None),
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
