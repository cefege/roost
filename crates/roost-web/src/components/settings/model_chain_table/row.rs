//! One keyed model-priority row, including drag handlers and its action menu.
//! `ModelChainTable` supplies the candidate and its current position; the editor
//! owns every mutation and persistence operation.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use dioxus::web::WebEventExt as _;

use crate::components::agent_chat::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconType, StatusDot, TableCell, TableRow,
};

#[component]
pub(super) fn ModelChainRow(
    token: String,
    model_name: String,
    thinking: Option<String>,
    unavailable: bool,
    options_available: bool,
    action_label: String,
    index: usize,
    row_count: usize,
    test_id: String,
    engines: bool,
    mut dragged_row: Signal<Option<usize>>,
    on_edit: EventHandler<usize>,
    on_reorder: EventHandler<(usize, usize)>,
    on_remove: EventHandler<usize>,
) -> Element {
    let grabbed = dragged_row() == Some(index);
    rsx! {
        TableRow {
            test_id: format!("{test_id}-row-{index}"),
            draggable: true,
            aria_grabbed: if grabbed { "true" } else { "false" },
            ondragstart: move |event: DragEvent| {
                dragged_row.set(Some(index));
                #[cfg(target_arch = "wasm32")]
                if let Some(transfer) = event.try_as_web_event()
                    .and_then(|native: web_sys::DragEvent| native.data_transfer())
                {
                    let _ = transfer.set_data("text/plain", &index.to_string());
                    transfer.set_effect_allowed("move");
                }
                #[cfg(not(target_arch = "wasm32"))]
                let _ = event;
            },
            ondragend: move |_| dragged_row.set(None),
            ondragover: move |event: DragEvent| {
                if dragged_row.peek().is_some() {
                    event.prevent_default();
                }
            },
            ondrop: move |event: DragEvent| {
                event.prevent_default();
                let source = dragged_row();
                dragged_row.set(None);
                if let Some(source) = source
                    && source < row_count
                    && source != index
                {
                    on_reorder.call((source, index));
                }
            },
            TableCell {
                div { class: "flex min-w-0 items-center gap-1.5 cursor-grab",
                    Icon {
                        icon: IconType::EllipsisVertical,
                        class: "size-4 shrink-0 text-muted-foreground",
                    }
                    span { class: "truncate text-xs font-medium",
                        if index == 0 {
                            if engines { "First" } else { "Default" }
                        } else {
                            "Fallback {index}"
                        }
                    }
                }
            }
            TableCell {
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    class: "min-w-0 max-w-full justify-start overflow-hidden text-left",
                    disabled: !options_available,
                    title: token,
                    "data-testid": "{test_id}-model-{index}",
                    onclick: move |_| on_edit.call(index),
                    if unavailable {
                        StatusDot {
                            status: "warn".to_owned(),
                            title: Some("Not available: this model is not in the catalog or its provider is not signed in".to_owned()),
                        }
                    }
                    span { class: "block min-w-0 truncate", "{model_name}" }
                    if let Some(level) = thinking {
                        span { class: "shrink-0 text-xs text-muted-foreground", "· {level}" }
                    }
                }
            }
            TableCell { class: "text-right",
                ModelChainRowActions {
                    test_id: format!("{test_id}-actions-{index}"),
                    label: action_label,
                    index,
                    row_count,
                    on_edit,
                    on_remove,
                    on_reorder,
                }
            }
        }
    }
}

#[component]
fn ModelChainRowActions(
    test_id: String,
    label: String,
    index: usize,
    row_count: usize,
    on_edit: EventHandler<usize>,
    on_remove: EventHandler<usize>,
    on_reorder: EventHandler<(usize, usize)>,
) -> Element {
    let trigger_id = use_hook(|| scoped_element_id("model-priority-actions"));
    let mut items = Vec::with_capacity(4);
    if index > 0 {
        items.push(ToolbarMenuItem::choice("up", "Move up", false));
    }
    if index + 1 < row_count {
        items.push(ToolbarMenuItem::choice("down", "Move down", false));
    }
    items.push(ToolbarMenuItem::choice("edit", "Change model", false));
    items.push(ToolbarMenuItem::choice("remove", "Remove", false));
    rsx! {
        ToolbarMenu {
            menu_key: test_id,
            trigger: ToolbarTrigger::Icon { icon: IconType::Ellipsis },
            aria_label: label,
            trigger_id: Some(trigger_id.clone()),
            items,
            on_choose: move |action: String| {
                let target = match action.as_str() {
                    "up" => index.checked_sub(1),
                    "down" if index + 1 < row_count => Some(index + 1),
                    "edit" => { on_edit.call(index); None },
                    "remove" => { on_remove.call(index); None },
                    _ => None,
                };
                if let Some(target) = target {
                    on_reorder.call((index, target));
                    #[cfg(target_arch = "wasm32")]
                    refocus_after_reorder(trigger_id.clone());
                }
            },
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn refocus_after_reorder(trigger_id: String) {
    use wasm_bindgen::{JsCast as _, closure::Closure};

    let Some(window) = web_sys::window() else {
        return;
    };
    // Moving a surviving DOM row can still blur its focused control.
    let focus = Closure::once_into_js(move || {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        if document
            .active_element()
            .is_none_or(|active| active.tag_name() == "BODY" || active.id() == trigger_id)
        {
            crate::components::ui::anchored_panel::focus_element_by_id(&trigger_id);
        }
    });
    let _ = window.request_animation_frame(focus.unchecked_ref());
}
