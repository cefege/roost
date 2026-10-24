//! Libero styles for the terminal's controller and touch key grid.

use dioxus::prelude::use_hook;
use libero::sx::{Sx, sx};

pub(super) struct NavKeyStyles {
    pub paper: Sx,
    pub grid: Sx,
    pub key: Sx,
    pub icon: Sx,
    pub label: Sx,
}

pub(super) fn use_nav_key_styles() -> NavKeyStyles {
    let paper = use_hook(|| {
        sx().position("fixed").z_index("40").box_sizing("border-box")
            .with("max-height", "max(0rem, calc(100dvh - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px) - 13.75rem - 1rem))")
            .with("right", "calc(env(safe-area-inset-right, 0px) + 1rem)")
            .with("bottom", "calc(env(safe-area-inset-bottom, 0px) + 13.75rem)")
            .overflow_y("auto").pointer_events("auto").font_family("sans-serif")
            .with("animation", "var(--animate-voice-dock-enter)")
            .media("(prefers-reduced-motion: reduce)", sx().with("animation", "none"))
            .selector("&[data-placement='tray']", sx().position("absolute").inset_inline("0")
                .with("bottom", "100%").with("height", "var(--term-tray-height)")
                .with("max-height", "none").overflow("visible"))
    });
    let grid = use_hook(|| {
        sx().display("grid").grid_template_columns("repeat(4, var(--term-chat-control))")
            .gap("xs").with("grid-template-areas", "'esc tab ctrl alt' 'home up end pgup' 'left down right pgdn' 'back space space ent' 'mouse mouse mouse mouse'")
            .selector("[data-placement='tray'] &", sx().grid_template_columns("repeat(9, minmax(0, 1fr))")
                .with("grid-template-areas", "'esc tab ctrl alt mouse home up end pgup' 'back space space ent ent left down right pgdn'"))
    });
    let key = use_hook(|| {
        sx().position("relative")
            .width("var(--term-chat-control)")
            .height("var(--term-chat-control)")
            .border_radius("md")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Sm))
            .with("transition", "box-shadow, transform 150ms")
            .selector(":active", sx().transform("scale(0.94)"))
            .selector("&[data-area='esc']", sx().grid_area("esc"))
            .selector("&[data-area='tab']", sx().grid_area("tab"))
            .selector("&[data-area='ctrl']", sx().grid_area("ctrl"))
            .selector("&[data-area='alt']", sx().grid_area("alt"))
            .selector("&[data-area='back']", sx().grid_area("back"))
            .selector("&[data-area='home']", sx().grid_area("home"))
            .selector("&[data-area='up']", sx().grid_area("up"))
            .selector("&[data-area='end']", sx().grid_area("end"))
            .selector("&[data-area='pgup']", sx().grid_area("pgup"))
            .selector("&[data-area='left']", sx().grid_area("left"))
            .selector("&[data-area='down']", sx().grid_area("down"))
            .selector("&[data-area='right']", sx().grid_area("right"))
            .selector("&[data-area='pgdn']", sx().grid_area("pgdn"))
            .selector(
                "&[data-area='enter']",
                sx().grid_area("ent")
                    .width("auto")
                    .background("primary")
                    .color("primary-contrast"),
            )
            .selector(
                "&[data-area='space']",
                sx().grid_area("space").width("auto"),
            )
            .selector(
                "&[data-area='mouse']",
                sx().grid_area("mouse").width("auto").gap("sm"),
            )
            .selector(
                "&[data-area='mouse'][data-active='true']",
                sx().background("primary").color("primary-contrast"),
            )
            .selector(
                "&[data-area='ctrl'][data-active='true'], &[data-area='alt'][data-active='true']",
                sx().background("secondary").color("secondary-contrast"),
            )
            .selector(
                "[data-placement='tray'] &",
                sx().height("var(--term-tray-key)").width("auto"),
            )
    });
    let icon = use_hook(|| {
        sx().position("relative")
            .z_index("1")
            .width("var(--term-nav-icon-size)")
            .height("var(--term-nav-icon-size)")
            .user_select("none")
    });
    let label = use_hook(|| {
        sx().position("relative")
            .z_index("1")
            .font_family("sans-serif")
            .with("font-size", "var(--term-nav-label-size)")
            .with("line-height", "var(--term-nav-label-line)")
            .font_weight("500")
            .letter_spacing("0.08em")
            .text_transform("uppercase")
            .user_select("none")
    });
    NavKeyStyles {
        paper,
        grid,
        key,
        icon,
        label,
    }
}
