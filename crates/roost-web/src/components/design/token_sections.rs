//! The `/design` gallery's token sections: colour swatches, the type scale, and
//! the spacing, radius and elevation scales. Each draws its specimens from the
//! `catalog` tables, whose classes are complete utility literals Tailwind
//! scans; `gallery.rs` mounts each inside a `GallerySection`.

use dioxus::prelude::*;

use super::catalog::{
    COLOR_GROUPS, ColorRole, ELEVATION_STEPS, SHAPE_STEPS, SPACE_BARS, SWATCH_GRID_CLASS, Specimen,
    TILE_GRID_CLASS, TYPE_STEPS,
};
use crate::components::ui::SectionTitle;

/// One colour role: a chip painted with it, and its name.
#[component]
fn Swatch(role: ColorRole) -> Element {
    rsx! {
        div { class: "flex min-w-0 flex-col gap-1",
            div { class: "h-12 rounded-sm border border-border {role.class}" }
            span { class: "font-mono text-xs break-words text-muted-foreground", "{role.name}" }
        }
    }
}

/// One type-scale step: its name and a sample line set in it.
#[component]
fn RampRow(step: Specimen) -> Element {
    rsx! {
        div { class: "flex items-baseline gap-4 border-b border-border py-2",
            span { class: "w-24 shrink-0 font-mono text-xs text-muted-foreground", "{step.label}" }
            span { class: "min-w-0 break-words text-foreground {step.class}",
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
            div { class: "mb-5",
                SectionTitle { {group.title} }
                div { class: SWATCH_GRID_CLASS,
                    for role in group.roles {
                        Swatch { role: *role }
                    }
                }
            }
        }
    }
}

/// The type scale, largest first.
#[component]
pub fn TypeRamp() -> Element {
    rsx! {
        for step in TYPE_STEPS {
            RampRow { step }
        }
    }
}

/// The spacing scale as bars of each width.
#[component]
pub fn SpacingScale() -> Element {
    rsx! {
        div { class: "flex flex-col gap-2",
            for bar in SPACE_BARS {
                div { class: "flex items-center gap-3",
                    span { class: "w-24 shrink-0 font-mono text-xs text-muted-foreground", "{bar.label}" }
                    div { class: bar.class }
                }
            }
        }
    }
}

/// The corner-radius scale.
#[component]
pub fn ShapeScale() -> Element {
    rsx! {
        div { class: SWATCH_GRID_CLASS,
            for shape in SHAPE_STEPS {
                div { class: "flex flex-col items-center gap-2",
                    div { class: shape.class }
                    span { class: "font-mono text-xs text-muted-foreground", "{shape.label}" }
                }
            }
        }
    }
}

/// The elevation scale.
#[component]
pub fn ElevationScale() -> Element {
    rsx! {
        div { class: TILE_GRID_CLASS,
            for step in ELEVATION_STEPS {
                div { class: step.class, "{step.label}" }
            }
        }
    }
}
