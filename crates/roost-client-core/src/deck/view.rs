//! What the deck paints this frame, derived from the folder's arrangement and
//! the measured deck box: the pane rects, which session paints in which slot,
//! the spotlight card, the parked terminal sizes and the phone tab order. Read
//! by the web `TerminalDeck` during render. Pure; ports the memos of
//! `apps/web/src/components/deck/terminal-deck-model.ts`.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::store::layout::{
    DividerRect, PaneLayout, PaneRect, PaneView, compact_leaf_for_layout, flat_tabs, layout_view,
    set_ratio,
};

use super::warm_set::WarmSet;

/// The deck element's measured box, in CSS px.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeckSize {
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// The panes and divider handles for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeckView {
    /// One view per painted pane, first-before-second.
    pub panes: Vec<PaneView>,
    /// The drag handles; empty on a compact host.
    pub dividers: Vec<DividerRect>,
}

/// Where one session paints this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSessionSlot {
    /// The pane box, strip included.
    pub rect: PaneRect,
    /// The pane the session paints in.
    pub pane_id: String,
    /// Whether that pane owns the keyboard.
    pub focused: bool,
    /// Whether this is the floated spotlight card.
    pub spotlit: bool,
}

/// The session the deck follows: the route's, or, while an overlay route
/// covers the deck, the one it showed last so parked renderers stay put.
pub fn deck_session_id(
    active_session_id: Option<&str>,
    surface_visible: bool,
    retained_session_id: Option<&str>,
) -> Option<String> {
    active_session_id
        .or(if surface_visible {
            None
        } else {
            retained_session_id
        })
        .map(str::to_owned)
}

/// The frame's panes. Nothing paints until the deck has been measured, a
/// divider mid-drag paints at its transient ratio, and a compact host paints
/// the one pane holding the followed session, full-bleed.
pub fn deck_view(
    layout: Option<&PaneLayout>,
    size: DeckSize,
    drag_ratios: &BTreeMap<String, f64>,
    compact: bool,
    active_session_id: Option<&str>,
) -> DeckView {
    let Some(layout) = layout else {
        return DeckView::default();
    };
    if size.w == 0.0 || size.h == 0.0 {
        return DeckView::default();
    }
    let mut current = Cow::Borrowed(layout);
    for (split_id, ratio) in drag_ratios {
        let root = set_ratio(&current.root, split_id, *ratio);
        current.to_mut().root = root;
    }
    if compact {
        let Some(leaf) = compact_leaf_for_layout(&current, active_session_id) else {
            return DeckView::default();
        };
        let selected_tab = match active_session_id {
            Some(active) if leaf.tabs.iter().any(|tab| tab == active) => active.to_owned(),
            _ => leaf.selected_tab.clone(),
        };
        return DeckView {
            panes: vec![PaneView {
                pane_id: leaf.pane_id.clone(),
                rect: PaneRect {
                    x: 0.0,
                    y: 0.0,
                    w: size.w,
                    h: size.h,
                },
                tab_ids: leaf.tabs.clone(),
                selected_tab,
                focused: true,
            }],
            dividers: Vec::new(),
        };
    }
    let (panes, dividers) = layout_view(&current, size.w, size.h);
    DeckView { panes, dividers }
}

/// The pane showing the floated session. A compact host never floats.
pub fn spotlight_pane<'view>(
    view: &'view DeckView,
    spotlight_session_id: Option<&str>,
    compact: bool,
) -> Option<&'view PaneView> {
    let session_id = spotlight_session_id?;
    if compact {
        return None;
    }
    view.panes
        .iter()
        .find(|pane| pane.selected_tab == session_id)
}

/// The centred card a floated pane paints at: the deck inset by 6% a side,
/// never less than 24px. `None` before the deck is measured.
pub fn spotlight_rect(size: DeckSize) -> Option<PaneRect> {
    if size.w == 0.0 || size.h == 0.0 {
        return None;
    }
    let margin_x = (size.w * 0.06).max(24.0);
    let margin_y = (size.h * 0.06).max(24.0);
    Some(PaneRect {
        x: margin_x,
        y: margin_y,
        w: size.w - 2.0 * margin_x,
        h: size.h - 2.0 * margin_y,
    })
}

/// Which session paints in which slot: each pane's selected tab, the floated
/// card over its pane, and on a compact host the swipe neighbour riding in
/// over the one pane.
pub fn slot_by_session(
    view: &DeckView,
    spotlight: Option<(&PaneView, PaneRect)>,
    swipe_neighbor_id: Option<&str>,
    compact: bool,
) -> BTreeMap<String, TerminalSessionSlot> {
    let mut slots: BTreeMap<String, TerminalSessionSlot> = BTreeMap::new();
    for pane in &view.panes {
        if !pane.selected_tab.is_empty() {
            slots.insert(
                pane.selected_tab.clone(),
                TerminalSessionSlot {
                    rect: pane.rect,
                    pane_id: pane.pane_id.clone(),
                    focused: pane.focused,
                    spotlit: false,
                },
            );
        }
    }
    if let Some((pane, rect)) = spotlight
        && !pane.selected_tab.is_empty()
    {
        slots.insert(
            pane.selected_tab.clone(),
            TerminalSessionSlot {
                rect,
                pane_id: pane.pane_id.clone(),
                focused: true,
                spotlit: true,
            },
        );
    }
    if let (Some(neighbor), true, Some(pane)) = (swipe_neighbor_id, compact, view.panes.first()) {
        slots.insert(
            neighbor.to_owned(),
            TerminalSessionSlot {
                rect: pane.rect,
                pane_id: pane.pane_id.clone(),
                focused: false,
                spotlit: false,
            },
        );
    }
    slots
}

/// The sessions the deck keeps mounted, in open-session order: every slotted
/// session plus the warm ones. A closed session is never mounted.
pub fn mounted_session_ids(
    open_session_ids: &[String],
    warm: &WarmSet,
    slots: &BTreeMap<String, TerminalSessionSlot>,
) -> Vec<String> {
    open_session_ids
        .iter()
        .filter(|id| warm.contains(id) || slots.contains_key(*id))
        .cloned()
        .collect()
}

/// The box every tab of every pane parks at: its pane's terminal area, so a
/// hidden renderer is already the size it will be revealed at and its scroll
/// maximum cannot move while frames arrive.
pub fn park_size_by_session(view: &DeckView, strip_height: f64) -> BTreeMap<String, DeckSize> {
    let mut sizes: BTreeMap<String, DeckSize> = BTreeMap::new();
    for pane in &view.panes {
        let terminal = DeckSize {
            w: pane.rect.w,
            h: (pane.rect.h - strip_height).max(0.0),
        };
        for tab in &pane.tab_ids {
            sizes.insert(tab.clone(), terminal);
        }
    }
    sizes
}

/// The folder's terminals in the flat order a phone walks (every pane, leaf
/// then tab); empty off compact, where the pane strips show them instead.
pub fn mobile_tab_ids(layout: Option<&PaneLayout>, compact: bool) -> Vec<String> {
    match layout {
        Some(layout) if compact => flat_tabs(&layout.root)
            .into_iter()
            .map(|tab| tab.tab_id)
            .collect(),
        _ => Vec::new(),
    }
}
