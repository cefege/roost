//! Theme-role swatches for the live design catalog.
//! The provider supplies their palette; this component declares no colour values.
//! Labels remain outside each swatch so the ink sample needs no guessed contrast.

use dioxus::prelude::*;
use libero::{
    components::{Box, Flex, Text},
    sx::sx,
};

const ROLES: [(&str, &str); 10] = [
    ("Surface", "surface"),
    ("Ink", "ink"),
    ("Neutral", "neutral"),
    ("Muted", "muted"),
    ("Primary", "primary"),
    ("Secondary", "secondary"),
    ("Success", "success"),
    ("Warning", "warning"),
    ("Error", "error"),
    ("Info", "info"),
];

#[component]
pub(super) fn ThemeRoles() -> Element {
    let grid = use_hook(|| {
        sx().display("grid")
            .gap("sm")
            .grid_template_columns("repeat(auto-fit,minmax(7rem,1fr))")
    });
    let swatches = use_hook(|| {
        ROLES.map(|(_, role)| {
            sx().width("100%")
                .height("2.5rem")
                .border_radius("sm")
                .background(role)
        })
    });
    rsx! {
        Box { sx: grid,
            for ((label, role), swatch) in ROLES.into_iter().zip(swatches) {
                Flex { direction: "column", gap: "xs",
                    Box { sx: swatch, aria_hidden: "true", "data-theme-role": role }
                    Text { size: "xs", color: "muted", "{label}" }
                }
            }
        }
    }
}
