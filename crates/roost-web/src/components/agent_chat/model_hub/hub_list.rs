//! The models hub's model-browsing markup: the sidebar of views, the kind tabs,
//! the list of model rows with their metric cells, and the detail lines for the
//! selected row. Pure rendering over `BrowseRow`s; the frame in `model_hub.rs`
//! owns the selection and handles the keys.

use dioxus::prelude::*;

use super::hub_browse::{BrowseRow, MODEL_KIND_TABS};
use super::hub_logic::{HubSection, HubView};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Icon, IconType, ListRow};

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
        div {
            class: "flex flex-col gap-1",
            role: "listbox",
            "aria-label": "Model views",
            tabindex: "0",
            "aria-activedescendant": format!("agent-model-hub-view-{}", current.id()),
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
                    let row_class = if locked { "text-muted-foreground" } else { "" };
                    rsx! {
                        ListRow {
                            key: "{section.view.id()}",
                            id: "agent-model-hub-view-{section.view.id()}",
                            role: "option",
                            aria_selected: selected.to_string(),
                            test_id: "agent-model-hub-view-{section.view.id()}",
                            selected: selected && focused,
                            dense: true,
                            class: row_class,
                            leading: rsx! { Icon { icon, class: "size-4" } },
                            headline: rsx! { "{section.label}" },
                            trailing: if count.is_empty() { None } else { Some(rsx! {
                                span { class: "text-xs tabular-nums text-muted-foreground", "{count}" }
                            }) },
                            onclick: move |_| on_select.call(view.clone()),
                        }
                    }
                }
            }
        }
    }
}

/// The icon for a view: sparkles for roles, a clock for recent, layers for all
/// models, a filled circle for a signed-in provider, a dashed one for a provider
/// still to sign in to.
fn view_icon(view: &HubView, locked: bool) -> IconType {
    match view {
        HubView::Roles => IconType::Sparkles,
        HubView::Recent => IconType::Clock,
        HubView::All => IconType::Layers,
        HubView::Provider(_) if locked => IconType::CircleDashed,
        HubView::Provider(_) => IconType::Circle,
    }
}

/// `Kind:` followed by the segment track of OMP's kind tabs; the active tab is
/// the one the list filters to.
#[component]
pub fn KindTabs(kinds: Vec<String>, active: String, on_select: EventHandler<String>) -> Element {
    rsx! {
        div { class: "flex flex-wrap items-center gap-1", role: "tablist", "aria-label": "Model kind",
            for kind in MODEL_KIND_TABS.iter().filter(|kind| kinds.iter().any(|known| known == *kind)) {
                {
                    let kind_value = (*kind).to_owned();
                    let selected = *kind == active;
                    let variant = if selected { ButtonVariant::Secondary } else { ButtonVariant::Ghost };
                    rsx! {
                        Button {
                            key: "{kind}",
                            variant,
                            size: ButtonSize::Sm,
                            class: "rounded-full",
                            role: "tab",
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
/// a ✓ on the chat's current model, and the metric cells right-aligned. The
/// metric cells are shown from the room the list has (`@container/rows`), so the
/// model name keeps its width on a narrow dialog.
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
            class: "@container/rows flex min-h-0 flex-1 flex-col overflow-y-auto overscroll-contain",
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
                    let row_id = row_option_id(&row.selector);
                    rsx! {
                        ListRow {
                            key: "{row.selector}",
                            id: "{row_id}",
                            role: "option",
                            aria_selected: is_selected.to_string(),
                            test_id: "agent-model-hub-row-{row.selector}",
                            selected: is_selected,
                            dense: true,
                            class: "whitespace-nowrap",
                            leading: rsx! {
                                span { class: "inline-flex size-4 items-center justify-center text-primary", "aria-hidden": "true",
                                    if is_selected {
                                        Icon { icon: IconType::ChevronRight, class: "size-4" }
                                    }
                                }
                            },
                            headline: rsx! {
                                span { class: "truncate",
                                    span { class: "text-muted-foreground", "{row.provider}/" }
                                    span { "{row.model_id}" }
                                    if !row.name.is_empty() && row.name != row.model_id {
                                        span { class: "text-muted-foreground", " {row.name}" }
                                    }
                                    if is_current {
                                        span { class: "text-success", "aria-label": "Current model", " ✓" }
                                    }
                                }
                            },
                            trailing: rsx! {
                                span { class: "flex gap-3 tabular-nums text-muted-foreground",
                                    if has_intel {
                                        span { class: "min-w-[4ch] text-right", "{row.intelligence}" }
                                    }
                                    if has_speed {
                                        span { class: "hidden min-w-[7ch] text-right @[38rem]/rows:inline", "{row.speed}" }
                                    }
                                    if has_context {
                                        span { class: "hidden min-w-[7ch] text-right @[24rem]/rows:inline", "{row.context}" }
                                    }
                                    if has_cost {
                                        span { class: "hidden min-w-[10ch] text-right @[30rem]/rows:inline", "{row.cost}" }
                                    }
                                }
                            },
                            onclick: move |_| {
                                if is_selected { on_activate.call(index) } else { on_select.call(index) }
                            },
                        }
                    }
                }
            }
        }
        if rows.is_empty() {
            p { class: "px-2 py-3 text-sm text-muted-foreground", "No models match." }
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
        div { class: "flex flex-wrap items-baseline text-sm text-muted-foreground", "data-testid": "agent-model-hub-detail",
            span { class: "font-semibold text-foreground", "{row.name}" }
            span { class: "whitespace-pre", " · " }
            span { "{price}" }
            for fact in facts.iter() {
                span { class: "whitespace-pre", " · " }
                span { "{fact}" }
            }
        }
        if !roles.is_empty() {
            div { class: "text-xs text-primary", "● {roles}" }
        }
    }
}
