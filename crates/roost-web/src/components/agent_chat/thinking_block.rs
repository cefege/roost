//! One reasoning block with a stable, accessible disclosure.
//! The transcript owns its identity; this component owns the reader's choice
//! so stream revisions cannot reopen or close a block unexpectedly.

use dioxus::prelude::*;
use libero::components::{Box, Button, Collapse, Flex, Indicator, Pictogram, Text};
use libero::sx::sx;

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::Markdown;

#[component]
pub(super) fn ThinkingBlock(text: String, live: bool, globally_visible: bool) -> Element {
    let mut expanded = use_signal(|| false);
    let mut manually_changed = use_signal(|| false);
    let disclosure_id = libero::hooks::use_id().read().clone();
    let started_ms = use_hook(now_ms);
    let mut observed_seconds = use_signal(|| 0_u64);
    let mut clock = use_future(use_reactive((&live,), move |(live,)| async move {
        if live {
            loop {
                crate::components::terminal::dom::sleep_ms(1_000).await;
                let elapsed = ((now_ms() - started_ms).max(0.0) / 1_000.0).floor() as u64;
                observed_seconds.set(elapsed);
            }
        }
    }));
    use_effect(use_reactive((&live,), move |(live,)| {
        if !live {
            clock.cancel();
            let elapsed = ((now_ms() - started_ms).max(0.0) / 1_000.0).floor() as u64;
            observed_seconds.set(elapsed);
            if !*manually_changed.peek() {
                expanded.set(false);
            }
        }
    }));
    let tail = if live {
        let lines: Vec<&str> = text.lines().collect();
        let first = lines.len().saturating_sub(8);
        lines[first..].join("\n")
    } else {
        text.clone()
    };
    let omitted_live_lines = live && text.lines().count() > 8;
    let observed = observed_seconds();
    let (copy_thought, thought_copied) = use_copy_clipboard(None);
    let copy_text = text.clone();
    let disclosure_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .with("border-left", "1px solid var(--border)")
            .padding_left("md")
            .color("muted")
    });
    let omission_sx = use_hook(|| sx().font_size("xs").color("muted"));
    let markdown_sx = use_hook(|| sx().color("muted"));
    let label = if live {
        format!(
            "Thinking… · {observed}s{}",
            if globally_visible { "" } else { " (hidden)" }
        )
    } else if observed > 0 {
        format!("Thought · observed {observed}s")
    } else {
        "Thought".to_owned()
    };
    rsx! {
        if globally_visible || live {
            section { "data-live": live.then_some("true"),
                Button {
                    variant: "standard",
                    color: "muted",
                    size: "sm",
                    disabled: !globally_visible,
                    aria_expanded: expanded() && globally_visible,
                    aria_controls: disclosure_id.clone(),
                    icon: rsx! { Pictogram { icon: pictogram_icons_lucide::brain::outlined } },
                    onclick: move |_| {
                        manually_changed.set(true);
                        expanded.toggle();
                    },
                    if live {
                        Flex { direction: "row", align: "center", gap: "xs",
                            Indicator { color: "primary", processing: true }
                            "{label}"
                        }
                    } else {
                        "{label}"
                    }
                    Pictogram {
                        icon: if expanded() && globally_visible { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined },
                    }
                }
                Collapse { id: disclosure_id, open: expanded() && globally_visible,
                    Box { component: "div", sx: disclosure_sx,
                        if omitted_live_lines {
                            Text { component: "p", sx: omission_sx, "Earlier lines are omitted from the live preview." }
                        }
                        Markdown { sx: markdown_sx, text: tail }
                        Button {
                            variant: "standard",
                            color: "primary",
                            onclick: move |_: MouseEvent| copy_thought(&copy_text),
                            if thought_copied() { "Copied" } else { "Copy thought" }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    crate::platform::browser::now_ms()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    0.0
}
