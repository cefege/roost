//! [`PadDom`] over the live document: focus facts from `activeElement`, the
//! untrusted synthetic keys, the explicit click / scroll / focus moves an
//! untrusted key cannot make, and the key pad's first-key focus retry — plus
//! [`dispatch_pad_actions`], the App's gamepad callback body, which runs the
//! router and arms the legend's idle hide. wasm32 only; the rules live in
//! `pad_router`. Ported from the DOM half of `apps/web/src/lib/padActions.ts`.

use dioxus::prelude::{ReadableExt as _, Signal, WritableExt as _};
use roost_web_terminal::reader_scroll::scroll_terminal_reader_box;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{
    Element, EventTarget, HtmlElement, KeyboardEvent, KeyboardEventInit, MouseEvent, MouseEventInit,
};

use crate::input_nav::dom_read;
use crate::input_nav::keypad_focus::start_first_key_focus;
use crate::input_nav::modality::NavModality;
use crate::input_nav::pad_bindings::PadAction;
use crate::input_nav::pad_router::{PAD_HINT_IDLE_MS, PadActionRouter};
use crate::input_nav::pad_surfaces::{
    KeypadFocusCancel, PadDom, PadFocus, PadSurfaces, SyntheticKey,
};

/// Run one poll's intents against the live document and `surfaces`, then arm
/// the legend's idle hide. The App passes this as `install_gamepad_source`'s
/// callback with the SHELL's [`PadSurfaces`].
pub fn dispatch_pad_actions(
    mut router: Signal<PadActionRouter>,
    modality: Signal<NavModality>,
    surfaces: &mut dyn PadSurfaces,
    actions: &[PadAction],
) {
    let pad_mode_active = modality.peek().pad_mode_active();
    let mut dom = BrowserPadDom::default();
    router.with_mut(|router| {
        router.run_pad_actions(
            actions,
            pad_mode_active,
            dom_read::now_ms(),
            &mut dom,
            surfaces,
        );
    });
    let deadline = {
        let router = router.peek();
        if !router.hints().visible {
            return;
        }
        router.hints_hide_at_ms()
    };
    // Every press re-arms a timer; only the one armed for the CURRENT deadline
    // hides the legend, so a superseded timer changes nothing and timer/clock
    // jitter cannot strand a visible legend.
    let hide = Closure::once_into_js(move || {
        let current = router
            .try_peek()
            .is_ok_and(|router| router.hints().visible && router.hints_hide_at_ms() == deadline);
        if current && let Ok(mut router) = router.try_write() {
            router.expire_hints(deadline);
        }
    });
    if let Some(window) = web_sys::window() {
        // `as i32` is exact: the idle window is a small whole number of ms.
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            hide.unchecked_ref(),
            PAD_HINT_IDLE_MS as i32,
        );
    }
}

/// The browser document, as the controller router sees it.
#[derive(Debug, Default)]
struct BrowserPadDom {
    // The element the last synthetic key was dispatched on: a click or a
    // textarea exit applies to the element that was focused when the key went
    // out, even if a handler moved focus since.
    last_key_target: Option<HtmlElement>,
}

impl PadDom for BrowserPadDom {
    fn focus(&self) -> PadFocus {
        let Some(active) = dom_read::active_element() else {
            return PadFocus::default();
        };
        PadFocus {
            present: true,
            terminal_box: dom_read::matches(&active, ".wterm"),
            in_keypad: dom_read::closest(&active, ".term-nav").is_some(),
            in_menu_or_dialog: dom_read::closest(&active, "[role=\"menu\"],[role=\"dialog\"]")
                .is_some(),
            pane_id: dom_read::closest(&active, "[data-pane-id]")
                .and_then(|pane| pane.get_attribute("data-pane-id")),
        }
    }

    fn press_key(&mut self, key: SyntheticKey) -> bool {
        let active = dom_read::active_element().and_then(|active| dom_read::html(&active));
        self.last_key_target = active.clone();
        let target: Option<EventTarget> = active.map(Into::into).or_else(|| {
            dom_read::document()
                .and_then(|document| document.body())
                .map(Into::into)
        });
        let Some(target) = target else {
            return true;
        };
        let init = KeyboardEventInit::new();
        init.set_key(key.as_str());
        init.set_bubbles(true);
        init.set_cancelable(true);
        let Ok(event) = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init) else {
            return true;
        };
        target.dispatch_event(&event).unwrap_or(true)
    }

    fn click_key_target(&mut self) {
        if let Some(target) = &self.last_key_target {
            target.click();
        }
    }

    fn leave_terminal_input(&mut self) -> bool {
        let Some(input) = self.last_key_target.as_ref() else {
            return false;
        };
        if !dom_read::matches(input, ".terminal-input") {
            return false;
        }
        let terminal_box =
            dom_read::closest(input, ".wterm").and_then(|element| dom_read::html(&element));
        let _ = input.blur();
        if let Some(terminal_box) = terminal_box {
            let _ = terminal_box.focus();
        }
        tracing::info!(target: "input_nav", "pad left the terminal input");
        true
    }

    fn scroll_focused_terminal_box(&mut self, delta_px: f64) -> bool {
        dom_read::active_element()
            .filter(|active| dom_read::matches(active, ".wterm"))
            .is_some_and(|active| scroll_terminal_reader_box(&active, delta_px))
    }

    fn scroll_pane_terminal_box(&mut self, pane_id: &str, delta_px: f64) {
        if let Some(terminal_box) = pane_terminal_box(pane_id) {
            scroll_terminal_reader_box(&terminal_box, delta_px);
        }
    }

    fn focus_pane_terminal_box(&mut self, pane_id: Option<&str>) {
        let scoped = pane_id.and_then(pane_terminal_box);
        let terminal_box = scoped.or_else(|| {
            dom_read::document()
                .and_then(|document| document.query_selector(".wterm").ok().flatten())
        });
        if let Some(html) = terminal_box.as_ref().and_then(dom_read::html) {
            let _ = html.focus();
        }
    }

    fn open_focused_context_menu(&mut self) {
        let Some(active) = dom_read::active_element() else {
            return;
        };
        let rect = dom_read::nav_rect(&active);
        let init = MouseEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        init.set_button(2);
        // The clientX/Y binding is integral; a sub-pixel centre is a rounding a
        // menu anchor cannot see.
        init.set_client_x((rect.left + rect.width / 2.0).round() as i32);
        init.set_client_y((rect.top + rect.height / 2.0).round() as i32);
        if let Ok(event) = MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init) {
            let _ = active.unchecked_ref::<EventTarget>().dispatch_event(&event);
        }
    }

    fn start_keypad_focus(&mut self) -> KeypadFocusCancel {
        start_first_key_focus()
    }
}

/// `pane_id`'s terminal scroll box.
fn pane_terminal_box(pane_id: &str) -> Option<Element> {
    let escaped = pane_id.replace('\\', "\\\\").replace('"', "\\\"");
    dom_read::document().and_then(|document| {
        document
            .query_selector(&format!("[data-pane-id=\"{escaped}\"] .wterm"))
            .ok()
            .flatten()
    })
}
