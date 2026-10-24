//! The screens shown before a protected surface exists: checking access and
//! refusing an untrusted browser. Pairing itself remains owned by PairSurface.
//! The checking frame replaces the whole application, so its state is explicit.

use dioxus::prelude::*;
use libero::{
    components::{Box, Paper, Pictogram, Text},
    sx::{Sx, sx},
};

fn checking_frame_sx() -> Sx {
    sx().display("flex")
        .min_height("100dvh")
        .width("100%")
        .align_items("center")
        .justify_content("center")
        .padding("lg")
}

/// The screen shown while the coordinator has not answered.
///
/// Full height and centred, because it replaces the whole application rather
/// than sitting inside the shell.
#[component]
pub fn CheckingScreen() -> Element {
    let tv = crate::components::layout::window_size::use_tv_layout();
    let base_frame_sx = use_hook(checking_frame_sx);
    let overscan_sx = use_hook(|| {
        sx().padding_top("var(--tv-overscan-block)")
            .padding_bottom("var(--tv-overscan-block)")
            .padding_left("var(--tv-overscan-inline)")
            .padding_right("var(--tv-overscan-inline)")
    });
    let frame_sx = if tv {
        base_frame_sx.and(overscan_sx)
    } else {
        base_frame_sx
    };
    let card_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("md")
            .padding("md")
    });
    rsx! {
        Box { sx: frame_sx, "data-testid": "access-checking",
            Paper { sx: card_sx, role: "status", aria_live: "polite",
                Pictogram { icon: pictogram_icons_lucide::hourglass::outlined, width: "1rem", height: "1rem" }
                Text { size: "sm", "Checking access…" }
            }
        }
    }
}

/// The refusal says that the key was rejected and offers the pairing route.
#[component]
pub fn UnauthorizedScreen(on_navigate: EventHandler<String>) -> Element {
    let pair_path = crate::routes::Route::Pair.to_path();
    let tv = crate::components::layout::window_size::use_tv_layout();
    let base_frame_sx = use_hook(checking_frame_sx);
    let overscan_sx = use_hook(|| {
        sx().padding_top("var(--tv-overscan-block)")
            .padding_bottom("var(--tv-overscan-block)")
            .padding_left("var(--tv-overscan-inline)")
            .padding_right("var(--tv-overscan-inline)")
    });
    let frame_sx = if tv {
        base_frame_sx.and(overscan_sx)
    } else {
        base_frame_sx
    };
    let card_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("md")
            .padding("md")
    });
    rsx! {
        Box { sx: frame_sx, "data-testid": "access-unauthorized",
            Paper { sx: card_sx, role: "alert",
                Pictogram { icon: pictogram_icons_lucide::lock::outlined, width: "1rem", height: "1rem" }
                Text { size: "sm", "This browser is not authorized on this coordinator." }
                libero::components::Anchor {
                    to: pair_path.clone(),
                    onclick: move |_| on_navigate.call(pair_path.clone()),
                    "data-testid": "access-unauthorized-pair",
                    "Pair this browser"
                }
            }
        }
    }
}
