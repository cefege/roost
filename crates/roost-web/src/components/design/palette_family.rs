//! The `/design` gallery's palette family: every base palette the theme catalog
//! ships, rust-ui and Omarchy alike. Each card paints the palette's catalog
//! swatch and names it. Applying one is Settings → Interface → Theme, because a
//! palette only paints the document root once it is the active choice.

use dioxus::prelude::*;

use super::catalog::CARD_GRID_CLASS;
use crate::components::ui::{Badge, BadgeVariant};
use crate::theme::catalog::base_themes;

/// One palette card: its swatch, label and id; Omarchy palettes carry a badge.
#[component]
fn PaletteCard(
    id: &'static str,
    label: &'static str,
    swatch: &'static str,
    omarchy: bool,
) -> Element {
    rsx! {
        div {
            class: "flex flex-col gap-2 rounded-card border border-border bg-card p-3",
            "data-palette": id,
            div { class: "flex items-center gap-2",
                span {
                    class: "size-4 rounded-control border border-border",
                    style: format!("background: {swatch};"),
                }
                span { class: "text-sm font-medium text-foreground", "{label}" }
                if omarchy {
                    Badge { variant: BadgeVariant::Secondary, "Omarchy" }
                }
            }
            span { class: "font-mono text-xs text-muted-foreground", "{id}" }
        }
    }
}

/// Every base palette, in picker order.
#[component]
pub fn PaletteFamily() -> Element {
    rsx! {
        div { class: CARD_GRID_CLASS, "data-testid": "design-palette-family",
            for theme in base_themes() {
                PaletteCard {
                    key: "{theme.id}",
                    id: theme.id,
                    label: theme.label,
                    swatch: theme.swatch,
                    omarchy: theme.omarchy.is_some(),
                }
            }
        }
    }
}
