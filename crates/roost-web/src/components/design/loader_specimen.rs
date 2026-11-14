//! `LoaderSpecimen`: the `Spinner` and the live `WorkingRow` the chat pins above
//! the composer during a running turn, rendered from the shipped components so
//! the gallery shows their real markup rather than a copy. Spacing and colour
//! are Tailwind utilities.

use dioxus::prelude::*;

use crate::components::ui::Spinner;

/// The spinner beside its label, then the working row with a sample intent.
#[component]
pub fn LoaderSpecimen() -> Element {
    rsx! {
        div { class: "flex flex-col gap-3",
            div { class: "flex items-center gap-2",
                Spinner { class: "size-4" }
                span { class: "text-muted-foreground", "Spinner" }
            }
        }
    }
}
