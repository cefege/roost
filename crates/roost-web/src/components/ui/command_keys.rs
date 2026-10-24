//! The command palette's keyboard rules and the DOM reads they need. Called by
//! `command.rs` (the root keydown) and `command_input.rs` (the active
//! descendant id); the rules are pure, the DOM helpers are wasm-only and no-op
//! natively, as `palette/list_keys.rs` does.
//!
//! The highlight is an item VALUE, not an index: the visible list is read from
//! the DOM at keydown time, so a filter change can never leave the highlight
//! pointing at a hidden item. Values must be unique within one `Command`.

/// What a key in the command does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandKeyAction {
    /// Move the highlight to this visible item.
    Highlight(String),
    /// Activate this visible item.
    Choose(String),
    /// Scroll the list back to its top (ArrowUp at the first item).
    ScrollToTop,
    /// Not a command key, or nothing to act on.
    Ignore,
}

/// A key pressed while the command has focus, given the visible item values in
/// order and the highlighted value. With no highlight, ArrowDown starts at the
/// first item and ArrowUp at the last; Enter has nothing to choose.
pub fn command_key_action(
    key: &str,
    visible_values: &[String],
    highlighted: Option<&str>,
) -> CommandKeyAction {
    if visible_values.is_empty() {
        return CommandKeyAction::Ignore;
    }
    let last = visible_values.len() - 1;
    let current = highlighted.and_then(|value| visible_values.iter().position(|v| v == value));
    match key {
        "ArrowDown" => match current.map_or(Some(0), |index| index.checked_add(1)) {
            Some(next) if next <= last => CommandKeyAction::Highlight(visible_values[next].clone()),
            _ => CommandKeyAction::Ignore,
        },
        "ArrowUp" => match current {
            Some(0) => CommandKeyAction::ScrollToTop,
            Some(index) => CommandKeyAction::Highlight(visible_values[index - 1].clone()),
            None => CommandKeyAction::Highlight(visible_values[last].clone()),
        },
        "Enter" => current.map_or(CommandKeyAction::Ignore, |index| {
            CommandKeyAction::Choose(visible_values[index].clone())
        }),
        _ => CommandKeyAction::Ignore,
    }
}

/// The DOM id of one item. Values are sanitised to id-safe characters so the
/// input's `aria-activedescendant` can name the highlighted item by id.
pub fn item_element_id(root_id: &str, value: &str) -> String {
    let sanitised: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("{root_id}-item-{sanitised}")
}

#[cfg(target_arch = "wasm32")]
fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

#[cfg(target_arch = "wasm32")]
fn item_element(root_id: &str, value: &str) -> Option<web_sys::HtmlElement> {
    use wasm_bindgen::JsCast as _;

    document()?
        .get_element_by_id(&item_element_id(root_id, value))?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

/// The values of the items currently shown, in DOM order.
#[cfg(target_arch = "wasm32")]
pub fn visible_item_values(root_id: &str) -> Vec<String> {
    use wasm_bindgen::JsCast as _;

    let Some(root) = document().and_then(|doc| doc.get_element_by_id(root_id)) else {
        return Vec::new();
    };
    let Ok(items) = root.query_selector_all("[data-name=\"CommandItem\"][data-visible=\"true\"]")
    else {
        return Vec::new();
    };
    (0..items.length())
        .filter_map(|index| items.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
        .filter_map(|element| element.get_attribute("data-value"))
        .collect()
}

/// Activate an item by running its click handler, as a pointer would.
#[cfg(target_arch = "wasm32")]
pub fn click_item(root_id: &str, value: &str) {
    if let Some(item) = item_element(root_id, value) {
        item.click();
    }
}

/// Bring an item into view with the least scrolling that shows it.
#[cfg(target_arch = "wasm32")]
pub fn scroll_item_into_view(root_id: &str, value: &str) {
    if let Some(item) = item_element(root_id, value) {
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        item.scroll_into_view_with_scroll_into_view_options(&options);
    }
}

/// Scroll the command's list back to its first item.
#[cfg(target_arch = "wasm32")]
pub fn scroll_list_to_top(root_id: &str) {
    let list = document()
        .and_then(|doc| doc.get_element_by_id(root_id))
        .and_then(|root| {
            root.query_selector("[data-name=\"CommandList\"]")
                .ok()
                .flatten()
        });
    if let Some(list) = list {
        list.set_scroll_top(0);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn visible_item_values(_root_id: &str) -> Vec<String> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn click_item(_root_id: &str, _value: &str) {}

#[cfg(not(target_arch = "wasm32"))]
pub fn scroll_item_into_view(_root_id: &str, _value: &str) {}

#[cfg(not(target_arch = "wasm32"))]
pub fn scroll_list_to_top(_root_id: &str) {}
