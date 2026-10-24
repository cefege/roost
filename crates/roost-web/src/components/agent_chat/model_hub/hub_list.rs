//! The models hub's model-browsing markup: the sidebar of views, the kind tabs,
//! the list of model rows with their metric cells, and the detail lines for the
//! selected row. Pure rendering over `BrowseRow`s; the frame in `model_hub.rs`
//! owns the selection and handles the keys.

use super::hub_browse::{BrowseRow, MODEL_KIND_TABS};
use super::hub_logic::{HubSection, HubView};
use dioxus::prelude::*;
use libero::components::{Button, Chip, Flex, List, Pictogram, SvgData, Text};
use libero::sx::sx;

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
    let list_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xs"));
    let row_sx = use_hook(|| sx().width("100%").justify_content("flex-start").gap("sm"));
    let icon_sx = use_hook(|| sx().flex_shrink("0").color("muted"));
    let label_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .with("overflow", "hidden")
            .with("text-overflow", "ellipsis")
            .with("white-space", "nowrap")
    });
    let count_sx = use_hook(|| {
        sx().flex_shrink("0")
            .color("muted")
            .with("font-variant-numeric", "tabular-nums")
    });
    rsx! {
        Flex {
            sx: list_sx,
            role: "listbox",
            aria_label: "Model views",
            tabindex: "0",
            aria_activedescendant: format!("agent-model-hub-view-{}", current.id()),
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
                    let option_id = format!("agent-model-hub-view-{}", section.view.id());
                    rsx! {
                        Button {
                            key: "{section.view.id()}",
                            sx: row_sx.clone(),
                            variant: if selected && focused { "tonal" } else { "standard" },
                            id: option_id.clone(),
                            role: "option",
                            aria_selected: if selected { "true" } else { "false" },
                            "data-testid": option_id,
                            onclick: move |_| on_select.call(view.clone()),
                            Flex { direction: "row", align: "center", sx: icon_sx.clone(),
                                Pictogram { icon, width: "1rem", height: "1rem" }
                            }
                            Text { sx: label_sx.clone(), size: "sm", color: if locked { "muted" } else { "ink" }, "{section.label}" }
                            if !count.is_empty() {
                                Text { sx: count_sx.clone(), size: "xs", "{count}" }
                            }
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
fn view_icon(view: &HubView, locked: bool) -> SvgData {
    match view {
        HubView::Roles => pictogram_icons_lucide::sparkles::outlined,
        HubView::Recent => pictogram_icons_lucide::clock::outlined,
        HubView::All => pictogram_icons_lucide::layers::outlined,
        HubView::Provider(_) if locked => pictogram_icons_lucide::circle_dashed::outlined,
        HubView::Provider(_) => pictogram_icons_lucide::circle::outlined,
    }
}

/// `Kind:` followed by the segment track of OMP's kind tabs; the active tab is
/// the one the list filters to.
#[component]
pub fn KindTabs(kinds: Vec<String>, active: String, on_select: EventHandler<String>) -> Element {
    let tabs_sx = use_hook(|| sx().gap("xs").flex_wrap("wrap"));
    rsx! {
        Flex { direction: "row", sx: tabs_sx, role: "group", "aria-label": "Model kind",
            for kind in MODEL_KIND_TABS.iter().filter(|kind| kinds.iter().any(|known| known == *kind)) {
                Chip {
                    key: "{kind}",
                    checked: active == *kind,
                    "data-testid": "agent-model-hub-kind-{kind}",
                    onclick: move |_| on_select.call((*kind).to_owned()),
                    "{kind}"
                }
            }
        }
    }
}

/// The test id on the model-rows listbox.
const MODEL_ROWS_TEST_ID: &str = "agent-model-hub-models-list";

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
    let marker_sx = use_hook(|| {
        sx().width("1rem")
            .height("1rem")
            .flex_shrink("0")
            .display("flex")
            .align_items("center")
            .justify_content("center")
            .color("primary")
    });
    let label_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .with("overflow", "hidden")
            .with("text-overflow", "ellipsis")
            .with("white-space", "nowrap")
    });
    let metrics_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .flex_shrink("0")
            .gap("sm")
            .color("muted")
            .with("font-variant-numeric", "tabular-nums")
    });
    let metric_sx = use_hook(|| sx().text_align("right").white_space("nowrap"));
    let empty_sx = use_hook(|| sx().padding_inline("sm").padding_block("md"));
    let rows_sx = use_hook(|| sx().min_width("0").max_height("16rem").overflow_y("auto"));
    let row_sx = use_hook(|| {
        sx().width("100%")
            .min_width("0")
            .justify_content("flex-start")
            .gap("sm")
    });
    let has_intel = rows.iter().any(|row| !row.intelligence.is_empty());
    let has_speed = rows.iter().any(|row| !row.speed.is_empty());
    let has_context = rows.iter().any(|row| !row.context.is_empty());
    let has_cost = rows.iter().any(|row| !row.cost.is_empty());
    let active_id = rows
        .get(selected)
        .map(|row| format!("agent-model-hub-row-{}", row.selector));
    rsx! {
        List {
            id: MODEL_ROWS_TEST_ID,
            "data-testid": MODEL_ROWS_TEST_ID,
            sx: rows_sx,
            role: "listbox",
            aria_label: "Models",
            tabindex: "0",
            aria_activedescendant: active_id,
            for (index, row) in rows.iter().cloned().enumerate() {
                {
                    let is_selected = index == selected;
                    let is_current = current.as_deref() == Some(row.selector.as_str());
                    let option_id = format!("agent-model-hub-row-{}", row.selector);
                    rsx! {
                        Button {
                            key: "{row.selector}",
                            sx: row_sx.clone(),
                            variant: if is_selected { "tonal" } else { "standard" },
                            id: option_id.clone(),
                            role: "option",
                            aria_selected: if is_selected { "true" } else { "false" },
                            tabindex: "-1",
                            onclick: move |_| {
                                if is_selected { on_activate.call(index) } else { on_select.call(index) }
                            },
                            Flex { direction: "row", align: "center", gap: "sm",
                                Flex { direction: "row", align: "center", justify: "center", sx: marker_sx.clone(), aria_hidden: "true",
                                    if is_selected {
                                        Pictogram { icon: pictogram_icons_lucide::chevron_right::outlined, width: "1rem", height: "1rem" }
                                    }
                                }
                                Flex { direction: "column", sx: label_sx.clone(),
                                    Text { size: "sm",
                                        Text { size: "sm", color: "muted", "{row.provider}/" }
                                        "{row.model_id}"
                                        if !row.name.is_empty() && row.name != row.model_id {
                                            Text { size: "sm", color: "muted", " {row.name}" }
                                        }
                                        if is_current {
                                            Text { size: "sm", color: "success", role: "img", aria_label: "Current model",
                                                Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "1rem", height: "1rem" }
                                            }
                                        }
                                    }
                                }
                                Flex { direction: "row", wrap: true, sx: metrics_sx.clone(),
                                    if has_intel { Text { sx: metric_sx.clone(), size: "xs", "{row.intelligence}" } }
                                    if has_speed { Text { sx: metric_sx.clone(), size: "xs", "{row.speed}" } }
                                    if has_context { Text { sx: metric_sx.clone(), size: "xs", "{row.context}" } }
                                    if has_cost { Text { sx: metric_sx.clone(), size: "xs", "{row.cost}" } }
                                }
                            }
                        }
                    }
                }
            }
        }
        if rows.is_empty() {
            Text { sx: empty_sx, size: "sm", color: "muted", "No models match." }
        }
    }
}

/// The selected row's detail: its facts on one line, and the roles it leads.
#[component]
pub fn ModelDetail(row: Option<BrowseRow>) -> Element {
    let detail_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("baseline")
    });
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
        Flex { direction: "row", wrap: true, sx: detail_sx, "data-testid": "agent-model-hub-detail",
            Text { size: "sm", "{row.name}" }
            Text { size: "sm", color: "muted",
                " · {price}"
                for fact in facts.iter() { " · {fact}" }
            }
        }
        if !roles.is_empty() {
            Text { size: "xs", color: "primary", "● {roles}" }
        }
    }
}
