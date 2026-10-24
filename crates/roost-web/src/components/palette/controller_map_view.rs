//! The controller map's diagram and held-button callouts.
//! The modal owner supplies current pad state; binding wording stays in its shared guide.
//! Constant Libero styles own the silhouette, not input routing or controller state.

use dioxus::prelude::*;
use libero::{
    components::{Box, Chip, Flex, Kbd, Paper, Text},
    sx::{Sx, sx},
};

use super::{
    FACE, PAD_MAP_CLUSTERS, PAD_MAP_SLOTS, UNBOUND_DETAIL, UNBOUND_LABEL, caption_for, guide_for,
    held_caps,
};
use crate::input_nav::PadHeld;
use crate::input_nav::pad_bindings::PadControlGuide;

#[component]
pub(super) fn ControllerMapBody(held: PadHeld) -> Element {
    let grid_sx = use_hook(|| {
        sx().display("grid")
            .grid_template_columns("minmax(0,1fr) minmax(0,0.7fr) minmax(0,1.3fr)")
            .grid_template_areas("'shoulder-left . shoulder-right' 'stick-left centre face' 'dpad centre stick-right'")
            .align_items("start")
            .gap("md")
            .padding("lg")
    });
    let clusters = use_hook(|| PAD_MAP_CLUSTERS.map(cluster_style));
    let caption_sx = use_hook(|| sx().margin("0").min_height("2rem"));
    let caps = held_caps(&held);
    let unnamed: Vec<usize> = held
        .buttons
        .iter()
        .copied()
        .filter(|index| {
            !PAD_MAP_SLOTS
                .iter()
                .any(|slot| slot.buttons.contains(index))
        })
        .collect();
    let caption = caption_for(&caps);
    rsx! {
        Flex { direction: "column", gap: "md", "data-testid": "controller-map",
            Paper { radius: "lg", shadow: "sm", sx: grid_sx,
                for (cluster, layout) in PAD_MAP_CLUSTERS.into_iter().zip(clusters) {
                    Box { sx: layout, "data-cluster": cluster,
                        for slot in PAD_MAP_SLOTS.iter().filter(|slot| slot.cluster == cluster) {
                            PadCallout {
                                cap: slot.cap.to_owned(),
                                held: caps.contains(slot.cap),
                                guide: guide_for(slot.cap),
                            }
                        }
                    }
                }
            }
            Text { size: "sm", color: "muted", sx: caption_sx,
                "data-testid": "controller-map-caption", {caption}
            }
            if !unnamed.is_empty() {
                Flex { direction: "row", wrap: true, align: "center", gap: "sm",
                    for index in unnamed {
                        PadCallout { cap: format!("Button {index}"), held: true, guide: None }
                    }
                }
            }
            Flex { direction: "row", wrap: true, align: "center", gap: "sm",
                Chip { "B, Esc or Start closes" }
                Text { size: "xs", color: "muted",
                    "Xbox labels — the same caps on any standard-mapping pad."
                }
            }
        }
    }
}

fn cluster_style(cluster: &'static str) -> Sx {
    let layout = sx()
        .display("flex")
        .flex_direction("column")
        .gap("sm")
        .grid_area(cluster);
    if cluster != FACE {
        return layout;
    }
    layout
        .display("grid")
        .grid_template_columns("repeat(3,minmax(0,1fr))")
        .grid_template_areas("'. face-y .' 'face-x . face-b' '. face-a .'")
        .selector("& > [data-cap='Y']", sx().grid_area("face-y"))
        .selector("& > [data-cap='X']", sx().grid_area("face-x"))
        .selector("& > [data-cap='B']", sx().grid_area("face-b"))
        .selector("& > [data-cap='A']", sx().grid_area("face-a"))
        .selector("& [data-detail]", sx().display("none"))
}

#[component]
fn PadCallout(cap: String, held: bool, guide: Option<PadControlGuide>) -> Element {
    let callout_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("start")
            .min_width("0")
            .gap("xs")
            .padding("sm")
            .box_sizing("border-box")
            .border_style("solid")
            .border_width("1px")
            .border_color("neutral.3")
            .selector(
                "&[data-held='true']",
                sx().border_color("primary").background("primary.1"),
            )
    });
    let label_sx = use_hook(|| sx().font_weight("500"));
    let detail = guide.map_or(UNBOUND_DETAIL, |guide| guide.detail);
    let label = guide.map_or(UNBOUND_LABEL, |guide| guide.label);
    let data_cap = cap.clone();
    rsx! {
        Paper { radius: "lg", sx: callout_sx, "data-cap": data_cap,
            "data-held": if held { "true" } else { "false" }, title: detail,
            Kbd { size: "sm", {cap} }
            Text { size: "sm", color: "ink", sx: label_sx, {label} }
            Text { size: "xs", color: "muted", "data-detail": "true", {detail} }
        }
    }
}
