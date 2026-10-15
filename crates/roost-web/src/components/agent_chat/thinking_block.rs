//! One reasoning block with a stable, accessible disclosure.
//! The transcript owns its identity; this component owns the reader's choice
//! so stream revisions cannot reopen or close a block unexpectedly.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, IconSize};

#[component]
pub(super) fn ThinkingBlock(text: String, live: bool, globally_visible: bool) -> Element {
    let mut expanded = use_signal(|| live);
    let mut manually_changed = use_signal(|| false);
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
    let copy_text = text.clone();
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
            section { class: "agent-chat__thinking", "data-live": live.then_some("true"),
                Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                class: "agent-chat__thinking-toggle",
                aria_expanded: (expanded() && globally_visible).to_string(),
                disabled: !globally_visible,
                onclick: move |_| {
                    manually_changed.set(true);
                    expanded.set(!expanded());
                },
                Icon { name: "psychology", size: IconSize::Sm }
                span { class: if live { "agent-chat__shimmer" } else { "" }, "{label}" }
                Icon { name: if expanded() && globally_visible { "expand_less" } else { "expand_more" }, size: IconSize::Sm }
            }
                if expanded() && globally_visible {
                div { class: "agent-chat__thinking-content",
                    if omitted_live_lines {
                        p { class: "agent-chat__thinking-tail-note", "Earlier lines are omitted from the live preview." }
                    }
                    div { class: "agent-chat__thinking-text agent-chat__markdown", dangerous_inner_html: super::markdown::markdown_to_safe_html(&tail) }
                    Button {
                        variant: ButtonVariant::Link,
                        class: "agent-chat__thinking-copy",
                        onclick: move |_: MouseEvent| { crate::components::notifications::clipboard::copy_text(&copy_text); },
                        "Copy thought"
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
