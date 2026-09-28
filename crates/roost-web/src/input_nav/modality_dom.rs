//! The browser half of the directional modality: read `?tv=` / `?pad=` and the
//! user agent, and write `data-tv` / `data-pad` onto `<html>` so `tv.css` and
//! `gamepad.css` key every override off one root attribute and no component
//! needs a TV or controller branch. Run before first paint by the App; the
//! choice setters are what Settings calls. Decisions live in `modality`.
//! Ported from the DOM halves of `apps/web/src/lib/{tvMode,padMode}.ts`.

use dioxus::prelude::{ReadableExt as _, Signal, WritableExt as _};
use roost_client_core::KeyValueStore;

use crate::input_nav::modality::{
    ModeChoice, NavModality, PAD_MODE_PARAM, TV_MODE_PARAM, matches_tv_user_agent,
    pointerless_tv_viewport,
};

/// Load the modality from the address bar, storage, and the user agent. The
/// UA is probed once: a TV never becomes a laptop mid-session, and the
/// predicate runs on every keydown.
pub fn load_nav_modality(storage: &dyn KeyValueStore) -> NavModality {
    let params = web_sys::window()
        .and_then(|window| window.location().search().ok())
        .and_then(|search| web_sys::UrlSearchParams::new_with_str(&search).ok());
    let param = |name: &str| params.as_ref().and_then(|params| params.get(name));
    let user_agent = web_sys::window()
        .and_then(|window| window.navigator().user_agent().ok())
        .unwrap_or_default();
    NavModality::load(
        param(TV_MODE_PARAM).as_deref(),
        param(PAD_MODE_PARAM).as_deref(),
        storage,
        matches_tv_user_agent(&user_agent),
    )
}

/// Write both root attributes, and log the modality they express.
pub fn apply_nav_modality(modality: &NavModality) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    else {
        return;
    };
    for (name, value) in modality.root_attributes() {
        let _ = root.set_attribute(name, value);
    }
    tracing::info!(
        target: "input_nav",
        choice = modality.tv_choice().as_str(),
        active = modality.tv_mode_active(),
        ua_match = modality.tv_user_agent(),
        pointer_none = pointerless_viewport(),
        "tv.mode"
    );
    tracing::info!(
        target: "input_nav",
        choice = modality.pad_choice().as_str(),
        active = modality.pad_mode_active(),
        seen = modality.pad_input_seen(),
        "pad.mode"
    );
}

/// Persist the controller choice and re-apply the root attribute at once.
pub fn set_pad_mode_choice(
    mut modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    modality.with_mut(|modality| modality.set_pad_choice(storage, choice));
    apply_nav_modality(&modality.peek());
}

/// Persist the TV choice and re-apply the root attribute at once.
pub fn set_tv_mode_choice(
    mut modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    modality.with_mut(|modality| modality.set_tv_choice(storage, choice));
    apply_nav_modality(&modality.peek());
}

/// Latch controller mode after real pad input. Writes the signal only on the
/// first press, so later presses do not re-render every modality reader.
pub(crate) fn note_pad_activity(mut modality: Signal<NavModality>) {
    if modality.peek().pad_input_seen() {
        return;
    }
    modality.with_mut(NavModality::note_pad_activity);
    apply_nav_modality(&modality.peek());
}

/// `(pointer: none)` on a TV-wide viewport. Diagnostic only; never switches
/// the mode (see `modality::matches_tv_user_agent`).
fn pointerless_viewport() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let pointer_none = window
        .match_media("(pointer: none)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches());
    let width = window
        .inner_width()
        .ok()
        .and_then(|width| width.as_f64())
        .unwrap_or(0.0);
    pointerless_tv_viewport(pointer_none, width)
}
