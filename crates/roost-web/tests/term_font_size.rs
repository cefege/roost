//! The terminal text-size controls' shared dispatch path: what one press of
//! smaller/larger/reset does to the store through a real `Pump`, where the
//! stepper's buttons stop, and which default a reset returns to.
//!
//! Test root, so the unwrap allowance is declared here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::store::prefs::terminal_font::{
    TERM_FONT_MAX_PX, TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX, TERMINAL_FONT_TV_DEFAULT_PX,
};
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::term_font_size::{
    TermFontStepState, device_default_term_font_px, reset_term_font, step_term_font,
};

thread_local! {
    /// The pump `root` built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// `Pump::new` reads a `Signal`, and only a real scope owns one.
fn root() -> Element {
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("term-font"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    rsx! {}
}

fn mounted() -> (VirtualDom, Pump) {
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    dom.render_immediate(&mut NoOpMutations);
    let pump = BUILT
        .with(|built| built.borrow_mut().take())
        .expect("the root builds a pump during the first rebuild");
    (dom, pump)
}

fn font_px(pump: &Pump) -> u32 {
    pump.core().borrow().store().prefs.term_font_px
}

#[test]
fn one_press_moves_the_size_one_pixel_whatever_the_caller_passes() {
    let (dom, pump) = mounted();
    let start = font_px(&pump);
    dom.in_runtime(|| step_term_font(&pump, 1));
    assert_eq!(font_px(&pump), start + 1);
    // A shortcut or a caller that passes a larger magnitude still moves one
    // step: the direction is the only thing a press carries.
    dom.in_runtime(|| step_term_font(&pump, 5));
    assert_eq!(font_px(&pump), start + 2);
    dom.in_runtime(|| step_term_font(&pump, -3));
    assert_eq!(font_px(&pump), start + 1);
}

#[test]
fn reset_returns_to_the_default_it_is_given() {
    let (dom, pump) = mounted();
    dom.in_runtime(|| {
        step_term_font(&pump, 1);
        step_term_font(&pump, 1);
    });
    assert_ne!(font_px(&pump), TERMINAL_FONT_DEFAULT_PX);
    dom.in_runtime(|| reset_term_font(&pump, device_default_term_font_px(false)));
    assert_eq!(font_px(&pump), TERMINAL_FONT_DEFAULT_PX);
    dom.in_runtime(|| reset_term_font(&pump, device_default_term_font_px(true)));
    assert_eq!(font_px(&pump), TERMINAL_FONT_TV_DEFAULT_PX);
}

#[test]
fn the_stepper_disables_each_end_exactly_at_the_bound() {
    let floor = TermFontStepState::at(TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX);
    assert!(!floor.can_shrink && floor.can_grow);
    let ceiling = TermFontStepState::at(TERM_FONT_MAX_PX, TERMINAL_FONT_DEFAULT_PX);
    assert!(ceiling.can_shrink && !ceiling.can_grow);
    let middle = TermFontStepState::at(TERMINAL_FONT_DEFAULT_PX, TERMINAL_FONT_DEFAULT_PX);
    assert!(middle.can_shrink && middle.can_grow && middle.at_default());
    assert!(
        !TermFontStepState::at(TERMINAL_FONT_DEFAULT_PX + 1, TERMINAL_FONT_DEFAULT_PX).at_default()
    );
}

#[test]
fn the_ten_foot_ui_resets_to_its_larger_default() {
    assert_eq!(device_default_term_font_px(false), TERMINAL_FONT_DEFAULT_PX);
    assert_eq!(
        device_default_term_font_px(true),
        TERMINAL_FONT_TV_DEFAULT_PX
    );
}
