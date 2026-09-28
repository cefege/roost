//! The deck's chrome above its terminals: on a desktop host each pane's tab
//! strip, the divider handles, the drop-zone highlight and the arrange menu; on
//! a compact host the phone bar, twice while a swipe slides the neighbour's in.
//! Called inline by `terminal_deck` (plain render functions, no hooks); every
//! gesture runs a `DeckOperations` command. Ports the chrome half of
//! `apps/web/src/components/deck/TerminalDeck.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::PaneView;

use super::arrange_menu::ArrangeMenu;
use super::deck_swipe::{Swipe, bar_neighbor_id};
use super::deck_swipe_style::swipe_style_for;
use super::inline_style::{InlineStyle, px};
use super::mobile_deck_bar::MobileDeckBar;
use super::pane_divider::PaneDivider;
use super::pane_strip::PaneStrip;
use super::terminal_deck_geometry::MOBILE_TERMINAL_STRIP_HEIGHT;
use super::terminal_deck_model::DeckFrame;
use super::terminal_deck_operations::{DeckOperations, DropOverlay};
use crate::motion::drop_zones::DropZone;

/// The desktop chrome for `frame`.
pub fn desktop_chrome(frame: &DeckFrame, operations: &DeckOperations, drop_overlay: Option<DropOverlay>) -> Element {
    let strip_height = frame.strip_height;
    let live_count = frame.folder.as_ref().map_or(0, |folder| folder.live_session_ids.len());
    let groups: Vec<PaneView> = frame
        .view
        .panes
        .iter()
        .filter(|pane| frame.spotlight_pane_id.as_deref() != Some(pane.pane_id.as_str()))
        .cloned()
        .collect();
    let arrange_ops = operations.clone();
    rsx! {
        for pane in groups {
            {pane_group(pane, operations.clone(), strip_height)}
        }
        for divider in frame.view.dividers.iter().cloned() {
            PaneDivider {
                key: "{divider.split_id}",
                divider,
                on_drag: {
                    let operations = operations.clone();
                    move |(split_id, ratio): (String, f64)| operations.divider_drag(split_id, ratio)
                },
                on_commit: {
                    let operations = operations.clone();
                    move |(split_id, ratio): (String, f64)| operations.divider_commit(split_id, ratio)
                },
            }
        }
        if let Some(overlay) = drop_overlay {
            div {
                class: "pane-drop-overlay workbench-pane-drop-overlay",
                "data-zone": zone_name(overlay.zone),
                style: "position: absolute; left: {px(overlay.rect.x)}; top: {px(overlay.rect.y)}; width: {px(overlay.rect.w)}; height: {px(overlay.rect.h)}; z-index: 5; pointer-events: none;",
            }
        }
        if live_count > 0 {
            div {
                class: "workbench-editor-actions",
                style: "position: absolute; top: 0; right: 0; height: {px(strip_height)}; display: flex; align-items: center; padding: 0 var(--md-space-2); z-index: 4;",
                ArrangeMenu {
                    can_arrange: live_count >= 2,
                    on_arrange: move |kind| arrange_ops.arrange(kind),
                }
            }
        }
    }
}

/// One pane's strip band.
fn pane_group(pane: PaneView, operations: DeckOperations, strip_height: f64) -> Element {
    let pane_id = pane.pane_id.clone();
    let rect = pane.rect;
    let select_ops = operations.clone();
    let close_ops = operations.clone();
    let reorder_ops = operations.clone();
    let new_tab_ops = operations.clone();
    let move_ops = operations.clone();
    let drop_ops = operations.clone();
    let end_ops = operations;
    let reorder_pane = pane_id.clone();
    let new_tab_pane = pane_id.clone();
    let move_pane = pane_id.clone();
    let drop_pane = pane_id.clone();
    rsx! {
        div {
            key: "{pane_id}",
            class: "workbench-editor-group",
            "data-pane": "true",
            "data-pane-id": pane_id.clone(),
            style: "position: absolute; left: {px(rect.x)}; top: {px(rect.y)}; width: {px(rect.w)}; height: {px(strip_height)}; z-index: 3;",
            if !pane.tab_ids.is_empty() {
                PaneStrip {
                    pane_id: pane_id.clone(),
                    tab_ids: pane.tab_ids.clone(),
                    selected_tab: pane.selected_tab.clone(),
                    focused: pane.focused,
                    on_select: move |session_id| select_ops.select(session_id),
                    on_close: move |session_id| close_ops.close(session_id),
                    on_reorder: move |ordered_ids| reorder_ops.reorder(reorder_pane.clone(), ordered_ids),
                    on_new_tab: move |()| new_tab_ops.new_tab(new_tab_pane.clone()),
                    on_tab_drag_move: move |(x, y): (f64, f64)| move_ops.tab_drag_move(&move_pane, x, y),
                    on_tab_tile_drop: Callback::new(move |(tab_id, x, y): (String, f64, f64)| {
                        drop_ops.tab_tile_drop(tab_id, &drop_pane, x, y)
                    }),
                    on_tab_drag_end: move |()| end_ops.tab_drag_end(),
                }
            }
        }
    }
}

/// The compact chrome: the phone bar for the route's session and, while a
/// slide runs, the neighbour's bar riding in beside it.
pub fn compact_chrome(
    frame: &DeckFrame,
    operations: &DeckOperations,
    swipe: Option<&Swipe>,
    active_session_id: &str,
    width: f64,
) -> Element {
    if frame.mobile_tabs.is_empty() {
        return rsx! {};
    }
    let neighbor = bar_neighbor_id(swipe, true).map(str::to_owned);
    let bar_style = |session_id: &str| {
        InlineStyle::new()
            .with("position", "absolute")
            .with("left", "0")
            .with("top", "0")
            .with("width", "100%")
            .with("height", px(MOBILE_TERMINAL_STRIP_HEIGHT))
            .with("z-index", "3")
            .merged(&swipe_style_for(swipe, session_id, width))
            .css()
    };
    rsx! {
        div { "data-testid": "mobile-strip-wrap", style: bar_style(active_session_id),
            {mobile_bar(frame, operations.clone(), active_session_id.to_owned())}
        }
        if let Some(neighbor) = neighbor {
            div { "data-testid": "mobile-strip-wrap-neighbor", style: bar_style(&neighbor),
                {mobile_bar(frame, operations.clone(), neighbor.clone())}
            }
        }
    }
}

fn mobile_bar(frame: &DeckFrame, operations: DeckOperations, selected_tab: String) -> Element {
    let focused_pane_id = frame.layout.as_ref().map(|layout| layout.focused_pane_id.clone()).unwrap_or_default();
    let select_ops = operations.clone();
    let close_ops = operations.clone();
    rsx! {
        MobileDeckBar {
            tab_ids: frame.mobile_tabs.clone(),
            selected_tab,
            on_select: move |session_id| select_ops.select(session_id),
            on_close: move |session_id| close_ops.close(session_id),
            on_new_tab: move |()| operations.new_tab(focused_pane_id.clone()),
        }
    }
}

/// v2's `DropZone` string, which the overlay CSS keys on.
fn zone_name(zone: DropZone) -> &'static str {
    match zone {
        DropZone::Center => "center",
        DropZone::Left => "left",
        DropZone::Right => "right",
        DropZone::Top => "top",
        DropZone::Bottom => "bottom",
        DropZone::Reorder => "reorder",
    }
}
