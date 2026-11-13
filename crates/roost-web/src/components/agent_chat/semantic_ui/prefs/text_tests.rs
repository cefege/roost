//! Preference input behavior at the rendered native edit boundary.
//! Real field events exercise delayed acknowledgments and masked drafts.
//! The fixture records renderer values and outbound OMP events without a browser.
#![allow(clippy::expect_used)]
use super::super::super::text_sync::TextSync;
use super::PrefText;
use dioxus::core::{AttributeValue, ElementId, Mutation, Mutations};
use dioxus::prelude::*;
use dioxus_html::{
    Code, Location, Modifiers, PlatformEventData, SerializedFormData, SerializedHtmlEventConverter,
    SerializedKeyboardData,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(PartialEq)]
struct Probe {
    initial: Value,
    source: RefCell<Option<Signal<Value>>>,
    events: RefCell<Vec<Value>>,
}

#[component]
fn FieldRoot(probe: Rc<Probe>) -> Element {
    let source = use_signal(|| probe.initial.clone());
    *probe.source.borrow_mut() = Some(source);
    let error = use_signal(|| None::<String>);
    let output = probe.clone();
    let send = use_callback(move |value: Value| output.events.borrow_mut().push(value));
    use_context_provider(|| TextSync::new(send, error));
    let snapshot = source.read();
    rsx! {
        PrefText {
            id: "settings", surface: "prefs", label: "Shell path",
            text: snapshot["draft"].as_str().unwrap_or_default().to_owned(),
            cursor: snapshot["cursor"].as_u64().and_then(|cursor| usize::try_from(cursor).ok()),
            ack: snapshot.get("ack").cloned(), placeholder: None,
            disabled: false, secret: snapshot["secret"] == true, mono: false, send,
        }
    }
}

struct FieldFixture {
    dom: VirtualDom,
    probe: Rc<Probe>,
    input_id: Option<ElementId>,
    value: String,
    placeholder: String,
}
impl FieldFixture {
    fn new(snapshot: Value) -> Self {
        dioxus_html::set_event_converter(Box::new(SerializedHtmlEventConverter));
        let probe = Rc::new(Probe {
            initial: snapshot,
            source: RefCell::new(None),
            events: RefCell::new(Vec::new()),
        });
        let mut dom = VirtualDom::new_with_props(
            FieldRoot,
            FieldRootProps {
                probe: probe.clone(),
            },
        );
        let mutations = dom.rebuild_to_vec();
        let mut fixture = Self {
            dom,
            probe,
            input_id: None,
            value: String::new(),
            placeholder: String::new(),
        };
        fixture.record(mutations);
        fixture.settle();
        fixture
    }
    fn record(&mut self, mutations: Mutations) {
        let mut current = None;
        for mutation in mutations.edits {
            match mutation {
                Mutation::PushId { id } | Mutation::SetId { id } => current = Some(id),
                Mutation::NewEventListener { name } if name == "input" => self.input_id = current,
                Mutation::SetAttribute {
                    name,
                    value: AttributeValue::Text(value),
                    ..
                } if name == "value" => self.value = value,
                Mutation::SetAttribute {
                    name,
                    value: AttributeValue::Text(value),
                    ..
                } if name == "placeholder" => self.placeholder = value,
                _ => {}
            }
        }
    }
    fn settle(&mut self) {
        for _ in 0..8 {
            let mutations = self.dom.render_immediate_to_vec();
            self.record(mutations);
            self.dom.process_events();
        }
    }
    fn input(&mut self, value: &str) {
        let data = PlatformEventData::new(Box::new(SerializedFormData::new(
            value.to_owned(),
            Vec::new(),
        )));
        self.dom.runtime().handle_event(
            "input",
            Event::new(Rc::new(data), true),
            self.input_id.expect("mounted preference input"),
        );
        self.settle();
    }
    fn key(&mut self, key: NamedKey, code: Code) {
        let data = PlatformEventData::new(Box::new(SerializedKeyboardData::new(
            Key::Named(key),
            code,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        )));
        self.dom.runtime().handle_event(
            "keydown",
            Event::new(Rc::new(data), true),
            self.input_id.expect("mounted preference input"),
        );
        self.settle();
    }
    fn native(&mut self, snapshot: Value) {
        let mut source = self.probe.source.borrow().expect("mounted source signal");
        self.dom.in_runtime(|| source.set(snapshot));
        self.settle();
    }
    fn edit(&self, index: usize) -> Value {
        self.probe
            .events
            .borrow()
            .iter()
            .filter(|event| event["params"]["event"]["ev"] == "edit")
            .nth(index)
            .cloned()
            .expect("native edit")
    }
}

#[test]
fn text_commit_and_cancel_wait_for_the_entire_native_draft() {
    // Native controls encode with kitty "report all keys" so OMP's decoder sees unambiguous keys.
    for (key, code, expected) in [
        (NamedKey::Enter, Code::Enter, "\x1b[13;1u"),
        (NamedKey::Escape, Code::Escape, "\x1b[27;1u"),
    ] {
        let mut field = FieldFixture::new(json!({"draft":"old","cursor":3}));
        field.input("new");
        let first = field.edit(0);
        field.input("newer");
        field.key(key, code);
        assert_eq!(field.value, "newer");
        assert!(
            field
                .probe
                .events
                .borrow()
                .iter()
                .all(|event| event["method"] != "input")
        );
        field.native(json!({"draft":"new","cursor":3,"ack":{"n":first["params"]["event"]["n"],"ok":true,"cursor":3}}));
        assert_eq!(
            field.value, "newer",
            "an older acknowledged draft cannot overwrite later typing"
        );
        let second = field.edit(1);
        assert!(
            field
                .probe
                .events
                .borrow()
                .iter()
                .all(|event| event["method"] != "input")
        );
        field.native(json!({"draft":"newer","cursor":5,"ack":{"n":second["params"]["event"]["n"],"ok":true,"cursor":5}}));
        let events = field.probe.events.borrow();
        let keys = events
            .iter()
            .filter(|event| event["method"] == "input")
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["params"]["data"], expected);
        assert!(
            events
                .iter()
                .all(|event| event["params"]["event"]["ev"] != "change"),
            "typing never saves a preference through a browser blur"
        );
    }
}

#[test]
fn secret_preference_never_copies_the_native_mask_into_the_edit() {
    let mut field = FieldFixture::new(json!({"draft":"••••","cursor":4,"secret":true}));
    assert_eq!(field.value, "");
    assert_eq!(field.placeholder, "••••");
    field.input("new-token");
    let edit = field.edit(0);
    assert_eq!(edit["params"]["event"]["text"], "new-token");
    assert_eq!(edit["params"]["event"]["from"], 0);
    assert_eq!(edit["params"]["event"]["to"], 4);
    assert_eq!(field.value, "new-token");
}
