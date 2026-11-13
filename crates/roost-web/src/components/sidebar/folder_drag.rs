//! Native drag data and boundary handling for folder rows.
//! FolderRow owns the visual state; these adapters keep child transitions
//! from clearing a valid target and claim only the app's folder drag.

use dioxus::prelude::*;

pub(super) fn prepare_folder_drag(event: &DragEvent) {
    #[cfg(target_arch = "wasm32")]
    if let Some(transfer) = native_drag_transfer(event) {
        transfer.set_effect_allowed("move");
        let _ = transfer.set_data("application/x-roost-folder", "folder");
        use dioxus::web::WebEventExt;
        use wasm_bindgen::JsCast as _;
        if let Some(row) = event
            .try_as_web_event()
            .and_then(|native| native.target())
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("[data-flip-key]").ok().flatten())
        {
            transfer.set_drag_image(&row, 20, 20);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = event;
}

pub(super) fn accept_folder_drag(event: &DragEvent) {
    event.prevent_default();
    event.stop_propagation();
    #[cfg(target_arch = "wasm32")]
    if let Some(transfer) = native_drag_transfer(event) {
        // The page-wide file/link guard rejects a drag until its owner claims it.
        transfer.set_drop_effect("move");
    }
}

pub(super) fn left_folder_row(event: &DragEvent) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt;
        use wasm_bindgen::JsCast as _;
        let Some(native) = event.try_as_web_event() else {
            return false;
        };
        let Some(row) = native
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("[data-flip-key]").ok().flatten())
        else {
            return false;
        };
        return !native
            .related_target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| row.contains(Some(&target)));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = event;
        true
    }
}

#[cfg(target_arch = "wasm32")]
fn native_drag_transfer(event: &DragEvent) -> Option<web_sys::DataTransfer> {
    use dioxus::web::WebEventExt;
    let native: web_sys::DragEvent = event.try_as_web_event()?;
    native.data_transfer()
}
