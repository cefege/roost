//! One selectable row of a command. Visibility is the filter result, written to
//! `data-visible` where the root's keyboard handler reads it back; the row never
//! owns keyboard state, it only reflects the highlighted value the root tracks.

use dioxus::prelude::*;
use icons::IconType;
use tw_merge::tw_merge;

use super::Icon;
use super::command::CommandContext;
use super::command_keys::item_element_id;

const ITEM_RECIPE: &str = "group relative flex gap-2 items-center px-2 py-1.5 text-sm rounded-sm cursor-default select-none outline-none data-[disabled=true]:pointer-events-none data-[disabled=true]:opacity-50 hover:bg-accent hover:text-accent-foreground aria-selected:bg-muted";
const CHECK_RESERVED: &str =
    "ml-auto size-4 text-muted-foreground opacity-0 group-aria-selected:opacity-100";
const CHECK_HIDDEN: &str = "ml-auto size-4 text-muted-foreground hidden group-aria-selected:block";

/// Whether an item with `value` survives the query. Matching is a
/// case-insensitive substring test; an empty query matches everything.
pub fn item_matches(value: &str, query: &str, should_filter: bool) -> bool {
    if !should_filter {
        return true;
    }
    let needle = query.to_lowercase();
    needle.is_empty() || value.to_lowercase().contains(&needle)
}

#[component]
pub fn CommandItem(
    children: Element,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] value: Option<String>,
    #[props(optional)] on_select: Option<EventHandler<()>>,
    #[props(default = false)] selected: bool,
    /// Reserve space for the check icon even when not highlighted, so rows align.
    #[props(default = false)]
    reserve_check_space: bool,
    /// Whether the row ends in the check icon. A palette row marks its highlight
    /// with `aria-selected` alone, so it opts out.
    #[props(default = true)]
    show_check: bool,
    #[props(into, optional)] test_id: Option<String>,
    /// Native attributes the caller forwards, such as `data-index` and
    /// `data-kind` for a host that reads the row back.
    #[props(extends = GlobalAttributes)]
    attributes: Vec<Attribute>,
) -> Element {
    let context = use_context::<CommandContext>();
    let value = value.unwrap_or_default();
    let query = context.search_query.read().clone();
    let is_visible = item_matches(&value, &query, context.should_filter);
    let is_highlighted = selected || context.highlighted.read().as_deref() == Some(value.as_str());

    let display = if is_visible { "flex" } else { "hidden" };
    let merged_class = tw_merge!(ITEM_RECIPE, display, class.as_deref().unwrap_or(""));
    let check_class = if reserve_check_space {
        CHECK_RESERVED
    } else {
        CHECK_HIDDEN
    };
    let item_id = item_element_id(&context.root_id, &value);
    let visible_flag = if is_visible { "true" } else { "false" };

    rsx! {
        div {
            id: "{item_id}",
            "data-name": "CommandItem",
            "data-value": "{value}",
            "data-visible": visible_flag,
            "data-testid": test_id.as_deref(),
            class: "{merged_class}",
            role: "option",
            tabindex: "0",
            "aria-selected": if is_highlighted { "true" } else { "false" },
            onclick: move |_| {
                if let Some(callback) = on_select {
                    callback.call(());
                }
            },
            ..attributes,
            {children}
            if show_check {
                Icon { icon: IconType::Check, class: check_class }
            }
        }
    }
}
