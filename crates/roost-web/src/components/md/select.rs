//! `Select`: the single-value select — a labelled trigger button and a floating
//! listbox. Ported from `apps/web/src/components/Settings/md/Select.tsx` and the
//! Kobalte select it wrapped; the settings panes (TV mode, agent, theme) and
//! editor forms compose it. `controls.css` owns the field, menu and validation
//! presentation.
//!
//! The value is the caller's: the trigger shows the option whose `value` equals
//! the `value` prop, and choosing reports the new value through `on_change` ONLY
//! when it differs — v2's guard, so re-choosing the current option is not a save.
//! A caller that rejects the change simply does not update `value`, and the
//! trigger keeps showing the old label (`agent-launcher-rejection.spec.ts`).

use std::rc::Rc;

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use super::class_list::class_list;
use super::dom::key_event_time_ms;
use super::form_field::{described_by, description_id, error_id, scoped_element_id};
use super::select_listbox::SelectListbox;
use super::select_navigation::{
    OpenFocus, TriggerKeyAction, Typeahead, initial_highlight, trigger_key_action,
    typeahead_character, typeahead_match,
};
use super::select_placement::AnchorRect;

/// The placeholder v2 showed when no option matches the value.
pub const DEFAULT_SELECT_PLACEHOLDER: &str = "Select an option";

/// One option: the value reported, and the label shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    /// What `on_change` receives.
    pub value: String,
    /// What the reader sees.
    pub label: String,
}

impl SelectOption {
    /// An option.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// The value to report when an option is chosen: its value, unless it is
/// already the current one.
pub fn select_change(options: &[SelectOption], current: &str, index: usize) -> Option<String> {
    options
        .get(index)
        .filter(|option| option.value != current)
        .map(|option| option.value.clone())
}

/// Whether the select is disabled: by the caller, or because it has nothing to
/// offer.
pub fn select_disabled(disabled: bool, option_count: usize) -> bool {
    disabled || option_count == 0
}

/// A select.
#[component]
pub fn Select(
    value: String,
    on_change: EventHandler<String>,
    label: Option<String>,
    options: Vec<SelectOption>,
    class: Option<String>,
    test_id: Option<String>,
    #[props(default)] disabled: bool,
    placeholder: Option<String>,
    description: Option<Element>,
    error: Option<Element>,
    #[props(default)] aria_invalid: bool,
    aria_described_by: Option<String>,
) -> Element {
    let base_id = use_hook(|| scoped_element_id("select"));
    let mut open = use_signal(|| false);
    let highlighted = use_signal(|| None::<usize>);
    let mut anchor = use_signal(|| None::<AnchorRect>);
    let mut trigger = use_signal(|| None::<Rc<MountedData>>);
    let mut pointer_type = use_signal(String::new);
    let mut typeahead = use_signal(Typeahead::default);

    let trigger_id = format!("{base_id}-trigger");
    let value_id = format!("{base_id}-value");
    let listbox_id = format!("{base_id}-listbox");
    let label_id = label.as_ref().map(|_| format!("{base_id}-label"));
    let description_element_id = description_id(&base_id);
    let error_element_id = error_id(&base_id);
    let selected = options.iter().position(|option| option.value == value);
    let is_disabled = select_disabled(disabled, options.len());
    let invalid = aria_invalid || error.is_some();
    let trigger_described_by = described_by(
        aria_described_by.as_deref(),
        description
            .is_some()
            .then_some(description_element_id.as_str()),
        (invalid && error.is_some()).then_some(error_element_id.as_str()),
    );
    let labelled_by = [label_id.as_deref(), Some(value_id.as_str())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let shown = selected.map(|index| options[index].label.clone());
    let placeholder_shown = shown.is_none().then_some("");
    let value_text = shown
        .unwrap_or_else(|| placeholder.unwrap_or_else(|| DEFAULT_SELECT_PLACEHOLDER.to_string()));
    let aria_controls = open().then(|| listbox_id.clone());
    let expanded_attribute = open().then_some("");
    let closed_attribute = (!open()).then_some("");
    let option_count = options.len();
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();

    let choose = {
        let options = options.clone();
        let value = value.clone();
        move |index: usize| {
            if let Some(next) = select_change(&options, &value, index) {
                tracing::debug!(target: "design", "select value chosen");
                on_change.call(next);
            }
        }
    };
    let choose_by_key = choose.clone();
    let mut open_listbox = move |focus: OpenFocus| {
        if is_disabled {
            return;
        }
        let mut highlighted = highlighted;
        highlighted.set(initial_highlight(option_count, selected, focus));
        open.set(true);
        if let Some(mounted) = trigger() {
            spawn(async move {
                if let Ok(rect) = mounted.get_client_rect().await {
                    anchor.set(Some(AnchorRect {
                        left: rect.origin.x,
                        top: rect.origin.y,
                        width: rect.size.width,
                        height: rect.size.height,
                    }));
                }
            });
        }
    };
    let mut close_listbox = move |refocus_trigger: bool| {
        open.set(false);
        anchor.set(None);
        if refocus_trigger && let Some(mounted) = trigger() {
            spawn(async move {
                let _ = mounted.set_focus(true).await;
            });
        }
    };
    let mut toggle = move |focus: OpenFocus| {
        if open() {
            close_listbox(true);
        } else {
            open_listbox(focus);
        }
    };

    rsx! {
        div {
            class: class_list(["roost-select", class.as_deref().unwrap_or("")]),
            role: "group",
            "data-expanded": expanded_attribute,
            "data-closed": closed_attribute,
            "data-invalid": invalid.then_some(""),
            "data-disabled": is_disabled.then_some(""),
            if let (Some(label), Some(label_id)) = (label, label_id.clone()) {
                span {
                    id: label_id,
                    class: "roost-select__label",
                    onclick: move |_| {
                        if !is_disabled && let Some(mounted) = trigger() {
                            spawn(async move {
                                let _ = mounted.set_focus(true).await;
                            });
                        }
                    },
                    {label}
                }
            }
            button {
                r#type: "button",
                id: trigger_id,
                class: "roost-select__trigger",
                disabled: is_disabled,
                "aria-haspopup": "listbox",
                "aria-expanded": open().to_string(),
                "aria-controls": aria_controls,
                "aria-labelledby": labelled_by,
                "aria-describedby": trigger_described_by,
                "aria-invalid": invalid.then_some("true"),
                "data-testid": test_id,
                "data-expanded": expanded_attribute,
                "data-closed": closed_attribute,
                onmounted: move |event: MountedEvent| trigger.set(Some(event.data())),
                onpointerdown: move |event: PointerEvent| {
                    let kind = event.pointer_type();
                    let primary = event.trigger_button() == Some(MouseButton::Primary);
                    if !is_disabled && kind != "touch" && primary {
                        event.prevent_default();
                        toggle(OpenFocus::First);
                    }
                    pointer_type.set(kind);
                },
                onclick: move |_| {
                    if !is_disabled && pointer_type() == "touch" {
                        toggle(OpenFocus::First);
                    }
                },
                onkeydown: move |event: KeyboardEvent| {
                        if is_disabled {
                            return;
                        }
                        let key = event.key().to_string();
                        let now = key_event_time_ms(&event);
                        let chord = event.modifiers().ctrl() || event.modifiers().meta();
                        let typing = typeahead_character(&key, chord)
                            .filter(|character| *character != " " || typeahead.peek().is_active(now));
                        if let Some(character) = typing {
                            let query = typeahead.write().push(character, now).to_string();
                            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                            if let Some(index) = typeahead_match(&label_refs, &query, selected) {
                                choose_by_key(index);
                            }
                            return;
                        }
                        match trigger_key_action(&key, option_count, selected) {
                            TriggerKeyAction::Open(focus) => {
                                event.prevent_default();
                                event.stop_propagation();
                                toggle(focus);
                            }
                            TriggerKeyAction::Choose(index) => {
                                event.prevent_default();
                                choose_by_key(index);
                            }
                            TriggerKeyAction::Ignore => {}
                        }
                },
                span {
                    id: value_id,
                    class: "roost-select__value",
                    "data-placeholder-shown": placeholder_shown,
                    {value_text}
                }
                span {
                    class: "roost-select__icon",
                    "aria-hidden": "true",
                    "data-expanded": expanded_attribute,
                    "data-closed": closed_attribute,
                    "\u{25BC}"
                }
            }
            if let Some(description) = description {
                div { id: description_element_id, class: "roost-select__description", {description} }
            }
            if let Some(error) = error.filter(|_| invalid) {
                div { id: error_element_id, class: "roost-select__error", {error} }
            }
            if let (true, Some(anchor)) = (open(), anchor()) {
                SelectListbox {
                    options,
                    selected,
                    highlighted,
                    anchor,
                    listbox_id,
                    label_id,
                    on_choose: move |index: usize| {
                        choose(index);
                        close_listbox(true);
                    },
                    on_dismiss: move |refocus: bool| close_listbox(refocus),
                }
            }
        }
    }
}
