//! The command root: owns the search query and the highlighted item value, and
//! turns arrow and Enter keys pressed inside it into highlight, scroll and
//! activation via `command_keys`. Items and input are its children and read
//! this context; the palette keeps its own window-level key handler.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::command_keys::{
    CommandKeyAction, click_item, command_key_action, scroll_item_into_view, scroll_list_to_top,
    visible_item_values,
};
use super::field::scoped_element_id;

const ROOT_RECIPE: &str = "flex overflow-hidden flex-col w-full h-full bg-transparent rounded-none text-popover-foreground";

#[derive(Clone)]
pub(super) struct CommandContext {
    pub(super) root_id: String,
    pub(super) search_query: Signal<String>,
    pub(super) should_filter: bool,
    pub(super) highlighted: Signal<Option<String>>,
    /// The caller's highlight, when it owns one. Read on every render so a caller
    /// whose key ladder moves the highlight never sees a stale value.
    pub(super) controlled_highlight: Option<String>,
}

impl CommandContext {
    /// The highlighted item's value: the caller's when it controls one, otherwise
    /// the root's own. Reading the signal subscribes the reader to the root's moves.
    pub(super) fn current_highlight(&self) -> Option<String> {
        match &self.controlled_highlight {
            Some(value) => Some(value.clone()),
            None => self.highlighted.read().clone(),
        }
    }
}

/// A command: a filterable list driven from the keyboard. `should_filter: false`
/// leaves filtering to the caller (server-side search).
#[component]
pub fn Command(
    children: Element,
    #[props(into, optional)] class: Option<String>,
    #[props(default = true)] should_filter: bool,
    /// Whether the root's own arrow, Home/End and Enter handling runs. A caller
    /// whose key ladder already owns those keys (wrap-around, a start index) turns
    /// it off, so the two handlers do not both act on one keypress.
    #[props(default = true)]
    handles_keys: bool,
    /// The highlighted item's value, when the caller owns the highlight. Absent,
    /// the root tracks its own.
    #[props(into, optional)]
    highlighted: Option<String>,
) -> Element {
    let root_id = use_hook(|| scoped_element_id("command"));
    let search_query = use_signal(String::new);
    let own_highlight = use_signal(|| None::<String>);
    // `provide_context` overwrites the value every render; `use_context_provider`
    // would keep the first one, and a later `highlighted` would never reach items.
    let context = provide_context(CommandContext {
        root_id: root_id.clone(),
        search_query,
        should_filter,
        highlighted: own_highlight,
        controlled_highlight: highlighted,
    });
    let merged_class = tw_merge!(ROOT_RECIPE, class.as_deref().unwrap_or(""));

    rsx! {
        div {
            id: "{root_id}",
            "data-name": "Command",
            class: "{merged_class}",
            tabindex: "-1",
            onkeydown: move |event: KeyboardEvent| {
                if handles_keys {
                    handle_command_key(&context, &event);
                }
            },
            {children}
        }
    }
}

fn handle_command_key(context: &CommandContext, event: &KeyboardEvent) {
    let visible_values = visible_item_values(&context.root_id);
    let current = context.current_highlight();
    let action = command_key_action(
        &event.key().to_string(),
        &visible_values,
        current.as_deref(),
    );
    let mut highlighted = context.highlighted;
    match action {
        CommandKeyAction::Highlight(value) => {
            event.prevent_default();
            scroll_item_into_view(&context.root_id, &value);
            highlighted.set(Some(value));
        }
        CommandKeyAction::Choose(value) => {
            event.prevent_default();
            click_item(&context.root_id, &value);
        }
        CommandKeyAction::ScrollToTop => {
            event.prevent_default();
            scroll_list_to_top(&context.root_id);
        }
        CommandKeyAction::Ignore => {}
    }
}
