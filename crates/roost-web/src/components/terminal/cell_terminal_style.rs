//! Libero presentation for the mounted terminal pane surface.

use dioxus::prelude::use_hook;
use libero::sx::{Sx, sx};

pub(super) fn use_pane_style() -> Sx {
    use_hook(|| {
        sx().position("absolute")
            .with("inset", "0")
            .display("flex")
            .flex_direction("column")
            .min_height("0")
            .overflow("hidden")
            .selector(
                "&[data-bell='ring']::after",
                sx().pointer_events("none")
                    .position("absolute")
                    .with("inset", "0")
                    .background("primary")
                    .opacity("0")
                    .with("content", "''")
                    .with("animation", "var(--animate-terminal-bell-flash)")
                    .media(
                        "(prefers-reduced-motion: reduce)",
                        sx().with("animation", "none")
                            .background("transparent")
                            .opacity("1")
                            .with("box-shadow", "inset 0 0 0 0.25rem var(--accent)"),
                    ),
            )
    })
}
