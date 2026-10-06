//! The one dispatch path for the terminal text size: the rail and drawer
//! steppers, the settings pane and the ⌘=/⌘-/⌘0 shortcuts all step and reset through here,
//! so "one step" and "this device's default" mean one thing everywhere; and
//! the one place the size reaches the page, which every mounted pane hears.
//! The size, its bounds and its persistence are
//! `roost_client_core::store::prefs::terminal_font`; `App` applies it.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::prefs::terminal_font::{
    TERM_FONT_MAX_PX, TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX, TERMINAL_FONT_TV_DEFAULT_PX,
};
use roost_client_core::store::shell_intent::ShellIntent;

use crate::input_nav::NavModality;
use crate::pump::Pump;

/// How far one press of a smaller/larger control moves the size, in pixels.
pub const TERM_FONT_STEP_PX: i32 = 1;

/// The window event `apply_term_font_size` raises after `--term-font-size`
/// moves. A pane caches its measured cell box, and a box that changes size
/// with no change of font face resizes nothing the pane observes, so without
/// this a larger size keeps the old column count and paints past its clip.
pub const TERM_FONT_SIZE_EVENT: &str = "roost-term-font-size";

/// The size a reset returns to: the ten-foot UI reads from across a room, so
/// it resets to the larger first-run size the host picked for it.
pub fn device_default_term_font_px(tv_mode: bool) -> u32 {
    if tv_mode {
        TERMINAL_FONT_TV_DEFAULT_PX
    } else {
        TERMINAL_FONT_DEFAULT_PX
    }
}

/// [`device_default_term_font_px`] for the mounted app, re-rendering the caller
/// when the ten-foot UI is switched. Outside the app root there is no modality,
/// and the desktop default applies.
pub fn use_device_default_term_font_px() -> u32 {
    let tv_mode = try_use_context::<Signal<NavModality>>()
        .is_some_and(|modality| modality.read().tv_mode_active());
    device_default_term_font_px(tv_mode)
}

/// One press of smaller (`direction < 0`) or larger (`direction > 0`).
pub fn step_term_font(pump: &Pump, direction: i32) {
    let delta = direction.signum() * TERM_FONT_STEP_PX;
    pump.dispatch(ClientEvent::Shell(ShellIntent::StepTermFont { delta }));
}

/// Return the size to `default_px`.
pub fn reset_term_font(pump: &Pump, default_px: u32) {
    pump.dispatch(ClientEvent::Shell(ShellIntent::ResetTermFont {
        default_px,
    }));
}

/// Push the size onto `<html>` (`--term-font-size`), then tell every mounted
/// pane to re-measure its cell. The first application precedes any pane, so a
/// pane's first measurement already reads it.
#[cfg(target_arch = "wasm32")]
pub fn apply_term_font_size(px: u32) {
    use wasm_bindgen::JsCast as _;
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Some(root) = window
        .document()
        .and_then(|document| document.document_element())
        .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = root
            .style()
            .set_property("--term-font-size", &format!("{px}px"));
    }
    if let Ok(event) = web_sys::Event::new(TERM_FONT_SIZE_EVENT) {
        let _ = window.dispatch_event(&event);
    }
    tracing::info!(target: "terminal", px, "terminal font size applied");
}

/// What a stepper can offer at a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermFontStepState {
    /// The current size, in pixels.
    pub px: u32,
    /// The size a reset returns to.
    pub default_px: u32,
    /// Smaller is still inside the bound.
    pub can_shrink: bool,
    /// Larger is still inside the bound.
    pub can_grow: bool,
}

impl TermFontStepState {
    /// The state at `px`, resetting to `default_px`.
    pub fn at(px: u32, default_px: u32) -> Self {
        Self {
            px,
            default_px,
            can_shrink: px > TERM_FONT_MIN_PX,
            can_grow: px < TERM_FONT_MAX_PX,
        }
    }

    /// Already at the size a reset returns to.
    pub fn at_default(&self) -> bool {
        self.px == self.default_px
    }
}
