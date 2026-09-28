//! The `/design` gallery's token sections: colour swatches, the type ramp, and
//! the spacing, shape and elevation scales. Ported from the `Swatch`, `RampRow`
//! and sections 1–5 of `apps/web/src/components/design/DesignGallery.tsx`;
//! `gallery.rs` mounts each inside a `GallerySection`. The catalogs they draw
//! are `catalog.rs`'s.

use dioxus::prelude::*;

use super::catalog::{
    COLOR_GROUPS, ELEV_STEPS, RAMP_STEPS, SHAPE_STEPS, SPACE_STEPS, gallery_grid_style,
};
use crate::components::md::SectionTitle;

/// The swatch grid's minimum column: two large spaces plus a medium one.
const SWATCH_COLUMN: &str = "calc(var(--md-space-9) * 2 + var(--md-space-6))";

/// A monospace caption in the label-small ramp, naming a token.
const TOKEN_CAPTION_STYLE: &str = "color: var(--text-mid); font-family: var(--font-mono); \
     font-size: var(--md-label-s-size); line-height: var(--md-label-s-line);";

/// One colour token: a chip painted with it and its name.
#[component]
fn Swatch(token: String) -> Element {
    rsx! {
        div { style: "display: flex; flex-direction: column; gap: var(--md-space-1); min-width: 0;",
            div {
                style: "height: calc(var(--md-space-7) + var(--md-space-6)); background: var({token}); \
                        border-radius: var(--md-shape-sm); \
                        border: var(--workbench-border-width) solid var(--md-outline-variant);",
            }
            span {
                style: "font-size: var(--md-label-s-size); line-height: var(--md-label-s-line); \
                        color: var(--text-mid); font-family: var(--font-mono); overflow-wrap: anywhere;",
                {token.clone()}
            }
        }
    }
}

/// One type-ramp step: its name and a sample line set in it.
#[component]
fn RampRow(step: String) -> Element {
    rsx! {
        div {
            style: "display: flex; align-items: baseline; gap: var(--md-space-4); \
                    padding: var(--md-space-2) 0; \
                    border-bottom: var(--workbench-border-width) solid var(--md-outline-variant);",
            span {
                style: "flex-shrink: 0; width: calc(var(--md-space-9) * 2 + var(--md-space-3)); \
                        font-size: var(--md-label-m-size); line-height: var(--md-label-m-line); \
                        color: var(--text-lo); font-family: var(--font-mono);",
                {step.clone()}
            }
            span {
                style: "color: var(--text-hi); font-size: var(--md-{step}-size); \
                        line-height: var(--md-{step}-line); font-weight: var(--md-{step}-weight); \
                        min-width: 0; overflow-wrap: anywhere;",
                "Roost — one tab, your whole fleet"
            }
        }
    }
}

/// Every colour role, grouped.
#[component]
pub fn ColorRoles() -> Element {
    rsx! {
        for group in COLOR_GROUPS {
            div { style: "margin-bottom: var(--md-space-5);",
                SectionTitle { {group.title} }
                div { style: gallery_grid_style(SWATCH_COLUMN),
                    for token in group.tokens {
                        Swatch { token: *token }
                    }
                }
            }
        }
    }
}

/// The type ramp, largest first.
#[component]
pub fn TypeRamp() -> Element {
    rsx! {
        for step in RAMP_STEPS {
            RampRow { step }
        }
    }
}

/// The spacing scale as bars of each width.
#[component]
pub fn SpacingScale() -> Element {
    rsx! {
        div { style: "display: flex; flex-direction: column; gap: var(--md-space-2);",
            for step in SPACE_STEPS {
                div { style: "display: flex; align-items: center; gap: var(--md-space-3);",
                    span {
                        style: "width: calc(var(--md-space-9) * 2); flex-shrink: 0; color: var(--text-lo); \
                                font-size: var(--md-label-m-size); line-height: var(--md-label-m-line); \
                                font-family: var(--font-mono);",
                        "--md-space-{step}"
                    }
                    div {
                        style: "height: var(--md-space-4); width: var(--md-space-{step}); \
                                background: var(--md-primary); border-radius: var(--md-shape-xs);",
                    }
                }
            }
        }
    }
}

/// The corner-radius scale.
#[component]
pub fn ShapeScale() -> Element {
    rsx! {
        div { style: gallery_grid_style(SWATCH_COLUMN),
            for shape in SHAPE_STEPS {
                div {
                    style: "display: flex; flex-direction: column; gap: var(--md-space-2); align-items: center;",
                    div {
                        style: "width: calc(var(--md-space-9) * 2 - var(--md-space-2)); \
                                height: calc(var(--md-space-9) * 2 - var(--md-space-2)); \
                                background: var(--md-primary-container); \
                                border: var(--workbench-border-width) solid var(--md-outline); \
                                border-radius: var(--md-shape-{shape});",
                    }
                    span { style: TOKEN_CAPTION_STYLE, "--md-shape-{shape}" }
                }
            }
        }
    }
}

/// The elevation scale.
#[component]
pub fn ElevationScale() -> Element {
    rsx! {
        div { style: gallery_grid_style("calc(var(--md-space-9) * 3)"),
            for step in ELEV_STEPS {
                div {
                    style: "height: calc(var(--md-space-9) * 2 - var(--md-space-2)); display: flex; \
                            align-items: center; justify-content: center; background: var(--surface-2); \
                            border-radius: var(--md-shape-md); box-shadow: var(--md-elev-{step}); \
                            color: var(--text-mid); font-family: var(--font-mono); \
                            font-size: var(--md-label-s-size); line-height: var(--md-label-s-line);",
                    "--md-elev-{step}"
                }
            }
        }
    }
}
