//! The models hub's model-browsing markup: the sidebar of views, the kind tabs,
//! the search field, the list of model rows with their metric cells, and the
//! detail lines for the selected row. Pure rendering over `BrowseRow`s; the
//! frame in `model_hub.rs` owns the selection and handles the keys.

use dioxus::prelude::*;

use super::hub_browse::{BrowseRow, MODEL_KIND_TABS};
use super::hub_logic::{HubSection, HubView};

/// The sidebar: Roles, Recent, All models, then each provider. Locked
/// providers are muted and mark the sign-in they need.
#[component]
pub fn HubSidebar(
    sections: Vec<HubSection>,
    current: HubView,
    focused: bool,
    roles_assigned: usize,
    roles_total: usize,
    on_select: EventHandler<HubView>,
) -> Element {
    rsx! {
        div { class: "model-hub__sidebar-list", role: "listbox", "aria-label": "Model views", tabindex: "0", "aria-activedescendant": format!("agent-model-hub-view-{}", current.id()),
            for section in sections.iter() {
                {
                    let view = section.view.clone();
                    let selected = section.view == current;
                    let locked = section.locked;
                    let icon = view_icon(&section.view, locked);
                    let count = if matches!(section.view, HubView::Roles) {
                        format!("{roles_assigned}/{roles_total}")
                    } else {
                        section.count.map(|count| count.to_string()).unwrap_or_default()
                    };
                    let mut row_class = String::from("model-hub__view");
                    if selected && focused { row_class.push_str(" model-hub__view--active"); }
                    if locked { row_class.push_str(" model-hub__view--locked"); }
                    rsx! {
                        div {
                            key: "{section.view.id()}",
                            id: "agent-model-hub-view-{section.view.id()}",
                            class: "{row_class}",
                            role: "option",
                            "aria-selected": selected.to_string(),
                            "data-testid": "agent-model-hub-view-{section.view.id()}",
                            onclick: move |_| on_select.call(view.clone()),
                            span { class: "model-hub__view-icon", "aria-hidden": "true", "{icon}" }
                            span { class: "model-hub__view-label", "{section.label}" }
                            if !count.is_empty() {
                                span { class: "model-hub__view-count", "{count}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The OMP sidebar glyph for a view: ✦ roles, ⏱ recent, ⬢ all, ● provider,
/// ○ a provider still to sign in to.
fn view_icon(view: &HubView, locked: bool) -> &'static str {
    match view {
        HubView::Roles => "✦",
        HubView::Recent => "⏱",
        HubView::All => "⬢",
        HubView::Provider(_) if locked => "○",
        HubView::Provider(_) => "●",
    }
}

/// `Kind:` followed by the segment track of OMP's kind tabs; the active tab is
/// the one the list filters to.
#[component]
pub fn KindTabs(kinds: Vec<String>, active: String, on_select: EventHandler<String>) -> Element {
    rsx! {
        div { class: "model-hub__kinds", role: "tablist", "aria-label": "Model kind",
            for kind in MODEL_KIND_TABS.iter().filter(|kind| kinds.iter().any(|known| known == *kind)) {
                {
                    let kind_value = (*kind).to_owned();
                    let selected = *kind == active;
                    rsx! {
                        button {
                            key: "{kind}",
                            r#type: "button",
                            role: "tab",
                            class: if selected { "model-hub__kind model-hub__kind--active" } else { "model-hub__kind" },
                            "aria-selected": selected.to_string(),
                            "data-testid": "agent-model-hub-kind-{kind}",
                            onclick: move |_| on_select.call(kind_value.clone()),
                            "{kind}"
                        }
                    }
                }
            }
        }
    }
}

/// The test id on the model-rows listbox. Formatted rather than literal so the
/// attribute is a dynamic mutation the mount harness can observe.
const MODEL_ROWS_TEST_ID: &str = "agent-model-hub-rows";

/// The model rows: a cursor on the selected row, `provider/` dimmed, the name,
/// a ✓ on the chat's current model, and the metric cells right-aligned.
#[component]
pub fn ModelRows(
    rows: Vec<BrowseRow>,
    selected: usize,
    current: Option<String>,
    on_select: EventHandler<usize>,
    on_activate: EventHandler<usize>,
) -> Element {
    let has_intel = rows.iter().any(|row| !row.intelligence.is_empty());
    let has_speed = rows.iter().any(|row| !row.speed.is_empty());
    let has_context = rows.iter().any(|row| !row.context.is_empty());
    let has_cost = rows.iter().any(|row| !row.cost.is_empty());
    let active_option = rows.get(selected).map(|row| row_option_id(&row.selector));
    rsx! {
        div {
            class: "model-hub__rows",
            id: "agent-model-hub-rows",
            "data-testid": "{MODEL_ROWS_TEST_ID}",
            role: "listbox",
            "aria-label": "Models",
            tabindex: "0",
            "aria-activedescendant": active_option,
            for (index, row) in rows.iter().enumerate() {
                {
                    let is_selected = index == selected;
                    let is_current = current.as_deref() == Some(row.selector.as_str());
                    let row_class = if is_selected { "model-hub__row model-hub__row--selected" } else { "model-hub__row" };
                    let row_id = row_option_id(&row.selector);
                    rsx! {
                        div {
                            key: "{row.selector}",
                            id: "{row_id}",
                            class: "{row_class}",
                            role: "option",
                            "aria-selected": is_selected.to_string(),
                            "data-testid": "agent-model-hub-row-{row.selector}",
                            onclick: move |_| {
                                if is_selected { on_activate.call(index) } else { on_select.call(index) }
                            },
                            span { class: "model-hub__cursor", "aria-hidden": "true",
                                if is_selected { "❯" }
                            }
                            span { class: "model-hub__name",
                                span { class: "model-hub__provider", "{row.provider}/" }
                                span { class: "model-hub__model", "{row.model_id}" }
                                if !row.name.is_empty() && row.name != row.model_id {
                                    span { class: "model-hub__display", " {row.name}" }
                                }
                                if is_current {
                                    span { class: "model-hub__current", "aria-label": "Current model", " ✓" }
                                }
                            }
                            span { class: "model-hub__metrics",
                                if has_intel {
                                    span { class: "model-hub__metric model-hub__metric--intel", "{row.intelligence}" }
                                }
                                if has_speed {
                                    span { class: "model-hub__metric model-hub__metric--speed", "{row.speed}" }
                                }
                                if has_context {
                                    span { class: "model-hub__metric model-hub__metric--ctx", "{row.context}" }
                                }
                                if has_cost {
                                    span { class: "model-hub__metric model-hub__metric--cost", "{row.cost}" }
                                }
                            }
                        }
                    }
                }
            }
        }
        if rows.is_empty() {
            p { class: "model-hub__empty", "No models match." }
        }
    }
}

/// The DOM id of a model row, which the listbox's `aria-activedescendant` names.
fn row_option_id(selector: &str) -> String {
    format!("agent-model-hub-row-{}", selector.replace('/', "-"))
}

/// The selected row's detail: its facts on one line, and the roles it leads.
#[component]
pub fn ModelDetail(row: Option<BrowseRow>) -> Element {
    let Some(row) = row else {
        return rsx! {};
    };
    let mut facts: Vec<String> = Vec::new();
    if !row.context.is_empty() {
        facts.push(row.context.clone());
    }
    if row.reasoning {
        facts.push("reasoning".to_owned());
    }
    if row.images {
        facts.push("vision".to_owned());
    }
    if !row.intelligence.is_empty() {
        facts.push(row.intelligence.clone());
    }
    if !row.speed.is_empty() {
        facts.push(row.speed.clone());
    }
    let price = match row.cost.as_str() {
        "variable" => "price varies".to_owned(),
        "unknown" => "pricing unknown".to_owned(),
        "free" | "included" => row.cost.clone(),
        cost => format!("{cost} per M"),
    };
    let roles = row.roles.join(" · ");
    rsx! {
        div { class: "model-hub__detail-facts", "data-testid": "agent-model-hub-detail",
            span { class: "model-hub__detail-name", "{row.name}" }
            span { class: "model-hub__detail-sep", " · " }
            span { "{price}" }
            for fact in facts.iter() {
                span { class: "model-hub__detail-sep", " · " }
                span { "{fact}" }
            }
        }
        if !roles.is_empty() {
            div { class: "model-hub__detail-roles", "● {roles}" }
        }
    }
}
