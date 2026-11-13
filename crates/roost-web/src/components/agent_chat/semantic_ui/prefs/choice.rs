//! Preference choices retain OMP's option order, labels and details in the menu;
//! the closed control shows the raw value, as the terminal's settings row does.
//! Menu visibility and preview highlight come from the owning prefs editor.
//! Stock popover and command primitives provide the menu without local filtering.
use super::super::{event, text_value};
use crate::components::ui::{
    ButtonSize, Command, CommandItem, CommandList, Popover, PopoverContent, PopoverTrigger, Tabs,
    TabsList, TabsTrigger, TabsVariant,
};
use dioxus::prelude::*;
use serde_json::{Value, json};

#[allow(clippy::too_many_arguments)]
pub(super) fn choice_control(
    control: &Value,
    prefs_id: &str,
    surface: &str,
    row_id: &str,
    label: &str,
    disabled: bool,
    editing: Option<&Value>,
    send: EventHandler<Value>,
) -> Element {
    let value = control
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let options = control.get("options").and_then(Value::as_array);
    let style = control
        .get("style")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let prefs_id = prefs_id.to_owned();
    let surface = surface.to_owned();
    let active =
        editing.filter(|editing| editing.get("row").and_then(Value::as_str) == Some(row_id));
    let row_id = row_id.to_owned();
    match style {
        "segmented" => rsx! {
            Tabs {
                value,
                on_value_change: move |value| if !disabled { send.call(event("change", &surface, &prefs_id, json!({"item":row_id,"value":value}))); },
                TabsList {
                    aria_label: Some(label.to_owned()),
                    variant: TabsVariant::Default,
                    for option in options.into_iter().flatten() {
                        {
                            let option_value = option.get("value").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let option_label = option.get("label").map(text_value).unwrap_or_else(|| option_value.clone());
                            let detail = option.get("detail").and_then(Value::as_str).map(str::to_owned);
                            rsx! { TabsTrigger { value: option_value, disabled, title: detail, has_panel: false, "{option_label}" } }
                        }
                    }
                }
            }
        },
        "menu" | "auto" => {
            let highlighted = active
                .and_then(|editing| editing.get("option"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let shown = value;
            let open_id = prefs_id.clone();
            let open_surface = surface.clone();
            let open_row = row_id.clone();
            rsx! {
                Popover {
                    open: active.is_some(),
                    on_open_change: move |open| if !disabled { send.call(event("action", &open_surface, &open_id, json!({"act":if open {"edit"} else {"close"},"value":open_row}))); },
                    PopoverTrigger { size: ButtonSize::Sm, disabled, aria_label: label.to_owned(), "{shown}" }
                    PopoverContent {
                        div {
                            inert: disabled.then_some(true),
                            onkeydown: move |key: KeyboardEvent| {
                                if disabled || key.is_composing() || key.key() == Key::Named(NamedKey::Escape) { return; }
                                if let Some(data) = super::super::keyboard::input_data(&key) {
                                    key.prevent_default();
                                    key.stop_propagation();
                                    send.call(json!({"method":"input","params":{"data":data}}));
                                }
                            },
                            Command {
                                should_filter: false,
                                handles_keys: false,
                                highlighted,
                                CommandList {
                                    for option in options.into_iter().flatten() {
                                        {
                                            let option_value = option.get("value").and_then(Value::as_str).unwrap_or_default().to_owned();
                                            let option_label = option.get("label").map(text_value).unwrap_or_else(|| option_value.clone());
                                            let detail = option.get("detail").and_then(Value::as_str);
                                            let select_id = prefs_id.clone();
                                            let select_surface = surface.clone();
                                            let select_item = format!("{row_id}={option_value}");
                                            let focus_id = select_id.clone();
                                            let focus_surface = select_surface.clone();
                                            let focus_item = select_item.clone();
                                            let change_id = prefs_id.clone();
                                            let change_surface = surface.clone();
                                            let change_row = row_id.clone();
                                            rsx! {
                                                div {
                                                    onpointerenter: move |_| if !disabled { send.call(event("select", &select_surface, &select_id, json!({"item":select_item}))); },
                                                    onfocusin: move |_| if !disabled { send.call(event("select", &focus_surface, &focus_id, json!({"item":focus_item}))); },
                                                    CommandItem {
                                                        value: option_value.clone(),
                                                        on_select: move |_| if !disabled { send.call(event("change", &change_surface, &change_id, json!({"item":change_row,"value":option_value}))); },
                                                        div {
                                                            class: "flex min-w-0 flex-col",
                                                            span { "{option_label}" }
                                                            if let Some(detail) = detail { span { class: "text-xs text-muted-foreground", "{detail}" } }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {
            rsx! { span { role: "alert", "data-omp-render-error": "unsupported-prefs-choice", class: "text-sm text-destructive", "Unsupported setting choice style: {style}" } }
        }
    }
}
