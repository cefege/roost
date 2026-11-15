//! Background swipe admission for the terminal deck, sidebar rows and drawer.
//! Mounted modal and floating-menu surfaces own horizontal gestures until they
//! unmount; their own internal gestures do not use this background-only gate.

/// Whether a mounted popup owns gestures instead of background navigation.
#[cfg(target_arch = "wasm32")]
pub fn overlay_owns_gesture() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| {
            document
                .query_selector("[role='dialog'], [role='alertdialog'], [role='menu'], [data-name='SelectGroup'][role='listbox']")
                .ok()
                .flatten()
        })
        .is_some()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn overlay_owns_gesture() -> bool {
    false
}
