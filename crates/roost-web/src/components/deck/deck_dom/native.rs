//! The native arm of `deck_dom`: no document, so no element to measure and
//! no listener to install. Same signatures as the browser arm.

use dioxus::prelude::*;

use super::{ClientBox, DeckPointerTarget, DeckTouch};
use crate::components::deck::pane_strip_drag::TabRect;
use crate::platform::browser_platform::ShortcutKey;

/// No element.
pub fn client_box(_mounted: &MountedData) -> Option<ClientBox> {
    None
}

/// No element.
pub fn client_size(_mounted: &MountedData) -> Option<(f64, f64)> {
    None
}

/// No computed style.
pub fn css_px_var(_mounted: &MountedData, _name: &str) -> f64 {
    0.0
}

/// No tabs laid out.
pub fn tab_rects(_rail: &MountedData) -> Vec<TabRect> {
    Vec::new()
}

/// Nothing overflows.
pub fn rail_overflowing(_rail: &MountedData) -> bool {
    false
}

/// Nothing to scroll.
pub fn reveal_active_tab(_rail: &MountedData) {}

/// Nothing to focus.
pub fn focus_tab_select(_session_id: &str) {}

/// No touch screen.
pub fn is_touch_device() -> bool {
    false
}

/// No motion preference.
pub fn reduced_motion() -> bool {
    false
}

/// No haptics.
pub fn vibrate(_duration_ms: u32) {}

/// No viewport.
pub fn viewport_width() -> f64 {
    0.0
}

/// No click to swallow.
pub fn swallow_next_click() {}

/// No drawer.
pub fn drawer_follow(_offset_px: f64) {}

/// No drawer.
pub fn drawer_settle_open(_commit: bool) {}

/// No target.
pub fn deck_pointer_target(_event: &PointerEvent) -> DeckPointerTarget {
    DeckPointerTarget::default()
}

/// No target.
pub fn chat_input_focus_pane(_event: &FocusEvent) -> Option<String> {
    None
}

/// No timer runs natively.
#[derive(Debug)]
pub struct Timeout;

impl Timeout {
    /// No timer.
    pub fn after(_delay_ms: u32, _callback: impl FnOnce() + 'static) -> Option<Self> {
        None
    }
}

/// No observer runs natively.
#[derive(Debug)]
pub struct SizeWatch;

impl SizeWatch {
    /// No observer.
    pub fn new(_on_resize: impl FnMut() + 'static) -> Option<Self> {
        None
    }

    /// Nothing to watch.
    pub fn watch(&self, _mounted: &MountedData) {}

    /// Nothing to watch.
    pub fn watch_tabs_only(&self, _rail: &MountedData) {}
}

/// No listener runs natively.
#[derive(Debug)]
pub struct Listeners;

impl Listeners {
    /// Nothing attached.
    pub fn remove(&self) {}

    /// No window.
    pub fn window_pointer_drag(
        _on_move: impl FnMut(f64, f64) + 'static,
        _on_up: impl FnMut(f64, f64) + 'static,
        _on_cancel: impl FnMut() + 'static,
    ) -> Option<Self> {
        None
    }

    /// No document.
    pub fn document_keys(_on_key: impl FnMut(&ShortcutKey) -> bool + 'static) -> Option<Self> {
        None
    }

    /// No element.
    pub fn deck_touches(_deck: &MountedData, _on_touch: impl FnMut(DeckTouch) -> bool + 'static) -> Option<Self> {
        None
    }
}

/// No menu.
pub fn run_menu_keys(_event: &KeyboardEvent, _menu_id: &str, _on_escape: impl FnOnce(), _on_tab: impl FnOnce() + 'static) {}

/// No menu.
pub fn focus_menu(_menu_id: &str, _edge: crate::components::context_menu::MenuFocusEdge) {}

/// No element.
pub fn focus_by_id(_id: &str) {}
