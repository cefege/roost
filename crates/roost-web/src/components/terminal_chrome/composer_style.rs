//! Libero styles for the terminal composer. Geometry remains expressed against
//! the workbench's measured terminal variables; this module owns no state.

use libero::sx::{Sx, sx};

pub(super) struct ComposerStyle {
    pub dock: Sx,
    pub box_style: Sx,
    pub attach: Sx,
    pub field: Sx,
    pub control: Sx,
    pub send: Sx,
}

pub(super) fn use_composer_style() -> ComposerStyle {
    let dock = dioxus::prelude::use_hook(|| {
        sx().position("fixed")
            .z_index("56")
            .display("flex")
            .flex_direction("column")
            .align_items("stretch")
            .gap("sm")
            .min_width("0")
            .font_family("sans-serif")
            .pointer_events("none")
            .selector("& > *", sx().flex_shrink("0"))
            .with(
                "left",
                "calc(var(--roost-main-left, 0px) + env(safe-area-inset-left, 0px) + 0.5rem)",
            )
            .with("right", "calc(env(safe-area-inset-right, 0px) + 0.5rem)")
            .with("bottom", "var(--term-chat-dock-offset)")
            .selector(
                "&[data-placement='pane']",
                sx().position("relative")
                    .with("left", "auto")
                    .with("right", "auto")
                    .with("bottom", "auto")
                    .margin_inline("sm")
                    .margin_bottom("sm")
                    .height("var(--term-chat-pane-rest, auto)")
                    .max_height("calc(100% - 0.5rem)")
                    .flex_shrink("0")
                    .justify_content("end")
                    .align_self("stretch")
                    .selector(
                        "&[data-size-constrained]",
                        sx().justify_content("start").overflow("auto"),
                    ),
            )
            .selector(
                "html[data-tv='true'] &[data-placement='viewport']",
                sx().with(
                    "left",
                    "calc(var(--roost-main-left, 0px) + var(--tv-overscan-inline) + 0.5rem)",
                )
                .with("right", "calc(var(--tv-overscan-inline) + 0.5rem)")
                .with(
                    "bottom",
                    "calc(var(--term-chat-dock-offset) + var(--tv-overscan-block))",
                ),
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().selector(
                    "&[data-composer-part='box']",
                    sx().with("animation", "none"),
                ),
            )
            .container("term-chat-pane")
    });
    let box_style = dioxus::prelude::use_hook(|| {
        sx().display("grid")
            .grid_template_columns("minmax(0, 1fr) auto auto")
            .border_radius("xl")
            .pointer_events("auto")
            .position("relative")
            .selector(
                "html[data-tv='true'] &[data-compact='false'][data-keys='true']",
                sx().grid_template_columns("auto minmax(0, 1fr) auto auto auto"),
            )
            .selector(
                "&[data-compact='true']",
                sx().grid_template_rows("auto var(--term-chat-control)"),
            )
            .selector(
                "&",
                sx().with("animation", "var(--animate-voice-dock-enter)"),
            )
            .container("term-chat-pane")
            .container_query(
                "term-chat-pane",
                "(max-width: 220px)",
                sx().selector(
                    "&[data-compact='false']",
                    sx().grid_template_columns("repeat(3, minmax(0, 1fr))")
                        .grid_template_rows("auto auto"),
                ),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 148px)",
                sx().selector(
                    "&[data-compact='false']",
                    sx().grid_template_columns("repeat(2, minmax(0, 1fr))")
                        .grid_template_rows("auto auto auto"),
                ),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 104px)",
                sx().selector(
                    "&[data-compact='false']",
                    sx().grid_template_columns("minmax(0, 1fr)")
                        .grid_template_rows("auto auto auto auto"),
                ),
            )
    });
    let attach = dioxus::prelude::use_hook(|| {
        sx().width("var(--term-chat-control)")
            .height("var(--term-chat-control)")
            .border_radius("9999rem")
            .grid_column("1")
            .grid_row("2")
            .selector("[data-compact='false'] &", sx().grid_row("1"))
            .container_query(
                "term-chat-pane",
                "(max-width: 220px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_row("2").justify_self("center"),
                ),
            )
    });
    let field = dioxus::prelude::use_hook(|| {
        sx().position("relative")
            .display("grid")
            .min_width("0")
            .grid_column("1 / -1")
            .grid_row("1")
            .selector("[data-compact='false'] &", sx().grid_column("2"))
            .container_query(
                "term-chat-pane",
                "(max-width: 220px)",
                sx().selector("[data-compact='false'] &", sx().grid_column("1 / -1")),
            )
    });
    let control = dioxus::prelude::use_hook(|| {
        sx().position("relative").z_index("1").box_sizing("border-box").min_height("var(--term-chat-control)")
            .width("100%").min_width("0").resize("none").border("0").background("transparent")
            .padding_inline("md").with("padding-block", "var(--term-chat-field-pad-block)")
            .font_family("sans-serif").with("font-size", "max(1rem, var(--term-chat-font-size))")
            .with("line-height", "var(--term-chat-line-height)").color("ink").outline("none")
            .with("field-sizing", "fixed").overflow_y("auto")
            .with("max-height", "min(calc(var(--term-chat-line-height) * var(--term-chat-field-max-lines) + 2 * var(--term-chat-field-pad-block)), calc(var(--term-chat-pane-height, calc(100dvh - max(var(--kb-offset, 0px), 0px))) * var(--term-chat-field-room-share)))")
            .selector("&::placeholder", sx().white_space("nowrap").color("muted"))
            .selector("&:focus-visible", sx().with("outline", "2px solid var(--lsx-focus-ring-color)").with("outline-offset", "-2px").border_radius("lg"))
            .selector("&[data-ghosted='true']", sx().color("transparent").with("caret-color", "var(--lsx-ink)")
                .selector("&::-webkit-scrollbar", sx().display("none")).selector("&", sx().with("scrollbar-width", "none")))
    });
    let send = dioxus::prelude::use_hook(|| {
        sx().width("var(--term-chat-control)")
            .height("var(--term-chat-control)")
            .border_radius("9999rem")
            .grid_column("3")
            .grid_row("2")
            .justify_self("center")
            .selector(
                "[data-compact='false'] &",
                sx().grid_column("4").grid_row("1"),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 220px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_column("3").grid_row("2"),
                ),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 148px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_column("1 / -1").grid_row("3"),
                ),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 104px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_column("1").grid_row("4"),
                ),
            )
    });
    ComposerStyle {
        dock,
        box_style,
        attach,
        field,
        control,
        send,
    }
}
