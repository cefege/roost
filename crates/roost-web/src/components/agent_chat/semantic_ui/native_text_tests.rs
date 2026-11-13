//! An idle mounted native field must stop rendering.
//! The field's flush effect writes optimistic text; if the effect also *reads*
//! the state through a tracked signal, its own write requeues the effect and a
//! cold browser mount never finishes. The probe is an actual `use_native_text`
//! hook in a real `VirtualDom`, with a hard render cap so a regression fails
//! instead of hanging the test process.
use super::super::text_sync::TextSync;
use super::{NativeTextSnapshot, use_native_text};
use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;

/// Far past the few renders a settled mount needs; a regression trips it.
const MAX_RENDERS: u32 = 64;
const SETTLE_ROUNDS: usize = 6;

#[component]
fn ProbeRoot(renders: Rc<Cell<u32>>) -> Element {
    let error = use_signal(|| None::<String>);
    let send = EventHandler::new(|_: Value| {});
    use_context_provider(|| TextSync::new(send, error));
    rsx! { FieldProbe{renders} }
}

#[component]
fn FieldProbe(renders: Rc<Cell<u32>>) -> Element {
    let count = renders.get() + 1;
    renders.set(count);
    assert!(count <= MAX_RENDERS, "an idle native field never settled");
    let send = EventHandler::new(|_: Value| {});
    let control = use_native_text(
        NativeTextSnapshot {
            id: "probe-field",
            surface: "probe-surface",
            text: "hello",
            cursor: Some(5),
            anchor: None,
            focused: false,
            readonly: false,
            typeahead: false,
            mask: false,
            ack: None,
            registration: "probe",
        },
        send,
    );
    rsx! { div{ "{control.value()}" } }
}

fn settle(dom: &mut VirtualDom) {
    for _ in 0..SETTLE_ROUNDS {
        dom.render_immediate(&mut NoOpMutations);
        dom.process_events();
    }
}

#[test]
fn an_idle_mounted_native_field_settles_without_self_retriggering() {
    let renders = Rc::new(Cell::new(0));
    let mut dom = VirtualDom::new_with_props(
        ProbeRoot,
        ProbeRootProps {
            renders: renders.clone(),
        },
    );
    dom.rebuild_in_place();
    settle(&mut dom);
    let settled = renders.get();
    assert!(settled >= 1, "the probe mounts and renders");
    assert!(
        settled <= 4,
        "an idle field settles after its first frame, not {settled} renders"
    );
    settle(&mut dom);
    assert_eq!(
        renders.get(),
        settled,
        "a later render pass must find nothing left to flush"
    );
}
