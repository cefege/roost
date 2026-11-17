//! The keyboard cursor's row-order publish, shared by the sidebar's two
//! views. Only the selected view publishes; `None` stays silent so the other
//! view's order is not overwritten, and an inactive sidebar publishes empty
//! so the arrows go back to native scroll.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::pump::Pump;

/// Publish the selected view's row order while it renders, and release the
/// cursor when the view unmounts.
pub(super) fn publish_cursor_targets(pump: &Pump, targets: Option<Vec<String>>) {
    let effect_pump = pump.clone();
    use_effect(use_reactive((&targets,), move |(targets,)| {
        if let Some(targets) = targets {
            effect_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(
                targets,
            )));
        }
    }));
    let drop_pump = pump.clone();
    use_drop(move || {
        drop_pump.dispatch(ClientEvent::Sidebar(SidebarIntent::PublishCursorTargets(
            Vec::new(),
        )));
    });
}
