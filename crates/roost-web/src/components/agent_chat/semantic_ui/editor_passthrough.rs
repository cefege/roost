//! Raw PTY input over a preserved native editor draft.
//! Keyboard bytes use the existing encoder; beforeinput and composition cover
//! phone keyboards and IME without admitting edits to the OMP draft.
use dioxus::prelude::*;
use serde_json::{Value, json};

pub(super) fn emit(data: String, send: EventHandler<Value>) {
    send.call(json!({"method":"input","params":{"data":data}}));
}

pub(super) fn use_input(
    element_id: &str,
    enabled: bool,
    value: &str,
    cursor: usize,
    send: EventHandler<Value>,
) -> Signal<bool> {
    let mut unavailable = use_signal(|| false);
    #[cfg(target_arch = "wasm32")]
    {
        use std::{cell::RefCell, rc::Rc};
        let owner = use_hook(|| Rc::new(RefCell::new(None::<browser::InputOwner>)));
        let mounted = owner.clone();
        let element_id = element_id.to_owned();
        let value = value.to_owned();
        use_effect(use_reactive(
            (&element_id, &enabled, &value, &cursor),
            move |(id, enabled, value, cursor)| {
                let mut mounted = mounted.borrow_mut();
                mounted.take();
                if enabled {
                    *mounted = browser::InputOwner::mount(id, value, cursor, send);
                    unavailable.set(mounted.is_none());
                } else {
                    unavailable.set(false);
                }
            },
        ));
        use_drop(move || {
            owner.borrow_mut().take();
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (element_id, enabled, value, cursor, send, &mut unavailable);
    unavailable
}

#[cfg(any(target_arch = "wasm32", test))]
fn input_bytes(kind: &str, data: Option<String>) -> Option<String> {
    match kind {
        "deleteContentBackward" => Some("\u{7f}".into()),
        "deleteContentForward" => Some("\u{1b}[3~".into()),
        "deleteWordBackward" => Some("\u{17}".into()),
        "deleteWordForward" => Some("\u{1b}d".into()),
        "deleteSoftLineBackward" => Some("\u{15}".into()),
        "deleteSoftLineForward" => Some("\u{0b}".into()),
        "insertLineBreak" | "insertParagraph" => Some("\r".into()),
        "insertText" | "insertReplacementText" => data.filter(|value| !value.is_empty()),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{emit, input_bytes};
    use dioxus::prelude::*;
    use serde_json::Value;
    use wasm_bindgen::{JsCast, closure::Closure};

    pub(super) struct InputOwner {
        element: web_sys::Element,
        before_input: Closure<dyn FnMut(web_sys::Event)>,
        composition_end: Closure<dyn FnMut(web_sys::Event)>,
    }

    impl InputOwner {
        pub(super) fn mount(
            id: String,
            value: String,
            cursor: usize,
            send: EventHandler<Value>,
        ) -> Option<Self> {
            let element = web_sys::window()?.document()?.get_element_by_id(&id)?;
            let before_input = Closure::new(move |event: web_sys::Event| {
                let Some(input) = event.dyn_ref::<web_sys::InputEvent>() else {
                    return;
                };
                let kind = input.input_type();
                if input.is_composing() || kind == "insertCompositionText" {
                    return;
                }
                event.prevent_default();
                if let Some(data) = input_bytes(&kind, input.data()) {
                    emit(data, send);
                }
            });
            let target = element.clone();
            let composition_end = Closure::new(move |event: web_sys::Event| {
                if let Some(data) = event
                    .dyn_ref::<web_sys::CompositionEvent>()
                    .and_then(web_sys::CompositionEvent::data)
                    .filter(|value| !value.is_empty())
                {
                    emit(data, send);
                }
                if let Some(input) = target.dyn_ref::<web_sys::HtmlInputElement>() {
                    input.set_value(&value);
                }
                if let Some(input) = target.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                    input.set_value(&value);
                }
                super::super::super::text_edit::mark_composing(&id, false);
                super::super::super::text_edit::set_selection(&id, Some(cursor), None);
            });
            let owner = Self {
                element,
                before_input,
                composition_end,
            };
            owner
                .element
                .add_event_listener_with_callback(
                    "beforeinput",
                    owner.before_input.as_ref().unchecked_ref(),
                )
                .ok()?;
            owner
                .element
                .add_event_listener_with_callback(
                    "compositionend",
                    owner.composition_end.as_ref().unchecked_ref(),
                )
                .ok()?;
            Some(owner)
        }
    }

    impl Drop for InputOwner {
        fn drop(&mut self) {
            let _ = self.element.remove_event_listener_with_callback(
                "beforeinput",
                self.before_input.as_ref().unchecked_ref(),
            );
            let _ = self.element.remove_event_listener_with_callback(
                "compositionend",
                self.composition_end.as_ref().unchecked_ref(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::input_bytes;

    #[test]
    fn phone_input_preserves_unicode_and_terminal_control_sequences() {
        assert_eq!(
            input_bytes("insertText", Some("日本語😀".into())).as_deref(),
            Some("日本語😀")
        );
        assert_eq!(
            input_bytes("deleteContentBackward", None).as_deref(),
            Some("\u{7f}")
        );
        assert_eq!(
            input_bytes("deleteContentForward", None).as_deref(),
            Some("\u{1b}[3~")
        );
        assert_eq!(input_bytes("insertParagraph", None).as_deref(), Some("\r"));
    }

    #[test]
    fn composition_and_clipboard_are_not_sent_twice() {
        assert!(input_bytes("insertCompositionText", Some("未確定".into())).is_none());
        assert!(input_bytes("insertFromComposition", Some("確定".into())).is_none());
        assert!(input_bytes("insertFromPaste", Some("pasted".into())).is_none());
    }
}
