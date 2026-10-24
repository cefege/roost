//! Navigation rules only for product pointer menus.
//! Libero owns button-menu behavior, popover placement and dismissal.
//! Terminal and sidebar pointer surfaces share immediate DOM navigation.

#[cfg(target_arch = "wasm32")]
mod dom;
pub mod return_focus;
pub mod terminal_menu;

#[cfg(target_arch = "wasm32")]
pub use dom::{focus_pointer_menu, run_pointer_menu_key};

#[cfg(not(target_arch = "wasm32"))]
pub fn focus_pointer_menu(_menu_id: &str) {}

#[cfg(target_arch = "wasm32")]
enum PointerMenuKeyAction {
    Tab,
    Focus(usize),
    Activate(usize),
    Ignore,
}

#[cfg(target_arch = "wasm32")]
fn pointer_menu_key_action(
    key: &str,
    current: Option<usize>,
    count: usize,
) -> PointerMenuKeyAction {
    if key == "Tab" {
        return PointerMenuKeyAction::Tab;
    }
    if count == 0 {
        return PointerMenuKeyAction::Ignore;
    }
    let last = count - 1;
    match (key, current) {
        ("ArrowDown", None) | ("Home", _) => PointerMenuKeyAction::Focus(0),
        ("ArrowDown", Some(index)) => PointerMenuKeyAction::Focus((index + 1) % count),
        ("ArrowUp", None) | ("End", _) => PointerMenuKeyAction::Focus(last),
        ("ArrowUp", Some(index)) => {
            PointerMenuKeyAction::Focus(if index == 0 { last } else { index - 1 })
        }
        ("Enter" | " ", Some(index)) => PointerMenuKeyAction::Activate(index),
        _ => PointerMenuKeyAction::Ignore,
    }
}
