//! The persistent command-palette host and the Start-button controller map.
//! Spotlight owns the modal, focus, dismissal and row navigation; Roost owns
//! its shortcut dispatcher, catalog, search semantics and action outcomes.

pub mod body;
pub mod controller_map;
pub mod copy_command_output;
pub mod dom;
pub mod outcome;

use dioxus::prelude::*;
use libero::components::{SpotlightOptions, use_spotlight};

use self::body::spotlight_actions;
use self::controller_map::ControllerMap;

/// The command palette, and the controller map that shares its mount point.
#[component]
pub fn CommandPalette() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    let pump = crate::pump::use_store();
    let navigate = crate::router_state::use_navigate();
    let path = crate::router_state::use_location();
    let options = SpotlightOptions {
        actions: Some(Callback::new({
            let pump = pump.clone();
            move |query: String| spotlight_actions(pump.clone(), overlays, navigate, path(), query)
        })),
        aria_label: Some("Command palette".to_owned()),
        placeholder: Some("Jump to session, workspace, or action…".to_owned()),
        shortcut: None,
        ..Default::default()
    };
    let spotlight = use_spotlight(options);
    let mut previously_open = use_signal(|| false);
    let mut product_open = overlays.palette;
    use_effect(move || {
        let requested_open = product_open();
        let spotlight_open = spotlight.is_open();
        if requested_open {
            if spotlight_open {
                previously_open.set(true);
            } else if previously_open() {
                product_open.set(false);
                previously_open.set(false);
            } else {
                spotlight.open();
                previously_open.set(true);
            }
        } else {
            if spotlight_open {
                spotlight.close();
            }
            previously_open.set(false);
        }
    });
    rsx! {
        ControllerMap {}
    }
}
