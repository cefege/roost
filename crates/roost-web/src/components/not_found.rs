//! The panel a route resolves to when the grammar does not know the path. It
//! names the path verbatim and offers only an explicit link home: a panel that
//! guessed a destination would be the redirect `Route::Unknown` exists to prevent.
//! Mounted by `app::RouteContent`; draws through `access_gate::GateCard`.

use dioxus::prelude::*;

use crate::components::access_gate::{CHECKING_CLASS, GateCard};
use crate::components::ui::{Icon, IconType};
use crate::routes::Route;

/// A path the grammar does not recognise.
#[component]
pub fn NotFound(path: String, on_navigate: EventHandler<String>) -> Element {
    let home_path = Route::Home.to_path();
    rsx! {
        section { class: CHECKING_CLASS, "data-testid": "route-not-found",
            GateCard { role: "status",
                Icon { icon: IconType::Link2Off, class: "size-4" }
                div {
                    span { "No route matches " }
                    code { {path} }
                    div {
                        a { href: home_path.clone(), onclick: move |_| on_navigate.call(home_path.clone()),
                            "data-testid": "route-not-found-home",
                            "Go to sessions"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{ServedSurface, Surface, surface_for};
    use crate::routes::Route;

    #[test]
    fn a_recognised_surface_path_and_a_typo_reach_different_panels() {
        // The two facts a reader has about their address bar, kept apart.
        assert!(matches!(
            surface_for(&Route::parse("/settings/machines")),
            Surface::Served(ServedSurface::Settings)
        ));
        assert!(matches!(
            surface_for(&Route::parse("/setttings")),
            Surface::NotFound { .. }
        ));
    }

    #[test]
    fn a_typo_in_a_known_prefix_is_a_not_found_and_not_a_served_surface() {
        // `/setttings` starts with the right letters. Treating it as a settings
        // surface with a typo would send a reader to a pane they did not open.
        assert!(matches!(
            surface_for(&Route::parse("/setttings/machines")),
            Surface::NotFound { .. }
        ));
    }
}
