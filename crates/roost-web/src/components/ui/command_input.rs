//! The command's search field: a combobox bound to the command's query. Typing
//! resets the highlight, so arrow keys start again from the first match. The
//! caller names the list it controls and labels the field.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::command::CommandContext;
use super::command_keys::item_element_id;

const INPUT_RECIPE: &str = "flex py-3 w-full h-10 text-sm bg-transparent rounded-md disabled:opacity-50 disabled:cursor-not-allowed placeholder:text-muted-foreground outline-hidden";

#[component]
pub fn CommandInput(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] aria_controls: Option<String>,
    /// The query the field shows, when the caller owns it (the route, for
    /// server-side search). Absent, the field keeps its own query.
    #[props(into, optional)]
    value: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default = false)] autofocus: bool,
    /// Callback fired when the search text changes. Use for server-side search.
    #[props(optional)]
    on_search_change: Option<EventHandler<String>>,
) -> Element {
    let context = use_context::<CommandContext>();
    let merged_class = tw_merge!(INPUT_RECIPE, class.as_deref().unwrap_or(""));
    let label = aria_label.unwrap_or_else(|| "Search".to_string());
    let query = value.unwrap_or_else(|| context.search_query.read().clone());
    let active_descendant = context
        .highlighted
        .read()
        .as_deref()
        .map(|value| item_element_id(&context.root_id, value));

    rsx! {
        input {
            "data-name": "CommandInput",
            "data-testid": test_id.as_deref(),
            class: "{merged_class}",
            autofocus,
            placeholder: placeholder.as_deref(),
            autocomplete: "off",
            spellcheck: "false",
            "aria-autocomplete": "list",
            role: "combobox",
            "aria-expanded": "true",
            "aria-controls": aria_controls.as_deref(),
            "aria-activedescendant": active_descendant.as_deref(),
            "aria-label": "{label}",
            r#type: "text",
            value: "{query}",
            oninput: move |event: FormEvent| {
                let value = event.value();
                let mut search_query = context.search_query;
                let mut highlighted = context.highlighted;
                search_query.set(value.clone());
                highlighted.set(None);
                if let Some(callback) = on_search_change {
                    callback.call(value);
                }
            },
            "data-1p-ignore": "true",
            "data-bwignore": "true",
            "data-lpignore": "true",
        }
    }
}
