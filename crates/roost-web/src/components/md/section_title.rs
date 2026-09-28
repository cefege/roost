//! `SectionTitle`: the caps label between cards. Ported from
//! `apps/web/src/components/Settings/md/SectionTitle.tsx`; settings panes and
//! the `/design` gallery compose it. `tokens.css` owns `.md-section-title`.

use dioxus::prelude::*;

/// A section label.
#[component]
pub fn SectionTitle(children: Element) -> Element {
    rsx! {
        div { class: "md-section-title", {children} }
    }
}
