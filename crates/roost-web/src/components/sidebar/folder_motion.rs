//! Measures visible folder positions at the moment a user drops a folder.
//! FolderList calls the returned callback before changing its manual order;
//! only that pending reorder animates, never visibility or activity changes.

use std::{cell::RefCell, rc::Rc};

use dioxus::prelude::*;

use crate::motion::grid_flip::{FlipSlots, flip_grid};

pub(super) fn use_folder_motion(keys: Rc<Vec<String>>) -> EventHandler<()> {
    let slots = use_hook(|| Rc::new(RefCell::new(FlipSlots::new())));
    let pending = Rc::clone(&slots);
    use_effect(use_reactive((&keys,), move |_| {
        let previous = std::mem::take(&mut *pending.borrow_mut());
        if !previous.is_empty() {
            if let Some(container) = folder_container() {
                flip_grid(&container, &previous);
            }
        }
    }));
    EventHandler::new(move |()| {
        if let Some(container) = folder_container() {
            *slots.borrow_mut() = flip_grid(&container, &FlipSlots::new());
        }
    })
}

fn folder_container() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .query_selector("[data-testid='folder-list']")
        .ok()
        .flatten()
}
