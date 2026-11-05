//! Scoped picker DOM addresses and contained reveal of the authoritative row.
//! Ids combine the surface, the picker node and the item so a browser can
//! address one row without a second selection model. Reveal scrolls only the
//! picker's own list, never an ancestor, and never moves the query focus.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use dioxus::prelude::*;

pub(super) fn picker_list_element_id(surface: &str, node: &str) -> String {
    picker_scoped_id(surface, node, "list")
}

pub(super) fn picker_item_element_id(surface: &str, node: &str, item: &str) -> String {
    picker_scoped_id(surface, node, &format!("item-{item}"))
}

fn picker_scoped_id(surface: &str, node: &str, suffix: &str) -> String {
    let context = try_consume_context::<super::super::NativeViewScope>();
    let view = context
        .as_ref()
        .map(|context| context.view_id.as_ref())
        .unwrap_or("");
    compose_picker_id(view, surface, node, suffix)
}

fn compose_picker_id(view: &str, surface: &str, node: &str, suffix: &str) -> String {
    let parts = [view, surface, node, suffix];
    let capacity = 16
        + parts
            .iter()
            .map(|part| part.len().div_ceil(3) * 4 + 1)
            .sum::<usize>();
    let mut id = String::with_capacity(capacity);
    id.push_str("omp-picker");
    for part in parts {
        id.push('.');
        URL_SAFE_NO_PAD.encode_string(part, &mut id);
    }
    id
}

#[cfg(any(target_arch = "wasm32", test))]
/// The scroll position that shows a row fully inside its own list, or `None`
/// when it is already visible. Callers apply it to the list element only.
pub(super) fn reveal_scroll_top(
    item_top: f64,
    item_bottom: f64,
    list_top: f64,
    list_bottom: f64,
    scroll_top: f64,
) -> Option<f64> {
    if item_top < list_top {
        Some((scroll_top + item_top - list_top).max(0.0))
    } else if item_bottom > list_bottom {
        Some((scroll_top + item_bottom - list_bottom).max(0.0))
    } else {
        None
    }
}

/// Reveals the native selection whenever OMP publishes a new one.
#[component]
pub(super) fn PickerReveal(selected: Option<String>, list_id: String, item_id: String) -> Element {
    use_effect(use_reactive(
        (&selected, &list_id, &item_id),
        move |(selected, list_id, item_id)| {
            if selected.is_some() {
                reveal_within_list(&list_id, &item_id);
            }
        },
    ));
    rsx! {}
}

#[cfg(target_arch = "wasm32")]
fn reveal_within_list(list_id: &str, item_id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let (Some(list), Some(item)) = (
        document.get_element_by_id(list_id),
        document.get_element_by_id(item_id),
    ) else {
        return;
    };
    if list.scroll_height() <= list.client_height() {
        return;
    }
    // A second pass lands the row after the rows skipped by content-visibility
    // adopt their real sizes; one pass can leave it just outside the viewport.
    for _ in 0..2 {
        let list_rect = list.get_bounding_client_rect();
        let item_rect = item.get_bounding_client_rect();
        let Some(next) = reveal_scroll_top(
            item_rect.top(),
            item_rect.bottom(),
            list_rect.top(),
            list_rect.bottom(),
            f64::from(list.scroll_top()),
        ) else {
            break;
        };
        list.set_scroll_top(next.round() as i32);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn reveal_within_list(_list_id: &str, _item_id: &str) {}

#[cfg(test)]
mod tests {
    use super::{compose_picker_id, reveal_scroll_top};

    #[test]
    fn reveal_scrolls_only_when_the_row_is_outside_the_list() {
        assert_eq!(reveal_scroll_top(120.0, 150.0, 100.0, 300.0, 0.0), None);
        assert_eq!(
            reveal_scroll_top(90.0, 150.0, 100.0, 300.0, 50.0),
            Some(40.0)
        );
        assert_eq!(
            reveal_scroll_top(280.0, 340.0, 100.0, 300.0, 20.0),
            Some(60.0)
        );
        assert_eq!(reveal_scroll_top(70.0, 90.0, 100.0, 300.0, 5.0), Some(0.0));
    }

    #[test]
    fn opaque_native_ids_cannot_alias_rows_lists_or_other_views() {
        let mut ids = std::collections::HashSet::new();
        for value in ["a:b", "a/b", "a b", "a_b", "a-b", "模型🔑"] {
            let id = compose_picker_id("view-a", "surface/a", "picker:1", &format!("item-{value}"));
            assert!(
                id.chars()
                    .all(|character| !character.is_whitespace() && character != '\0')
            );
            assert!(ids.insert(id));
        }
        assert!(ids.insert(compose_picker_id("view-a", "surface/a", "picker:1", "list")));
        assert_ne!(
            compose_picker_id("view-a", "s:0", "n:1", "item-x"),
            compose_picker_id("view-b", "s:0", "n:1", "item-x")
        );
        assert_ne!(
            compose_picker_id("view-a", "s:0", "n:1", "item-x"),
            compose_picker_id("view-a", "s:0", "n/1", "item-x")
        );
    }
}
