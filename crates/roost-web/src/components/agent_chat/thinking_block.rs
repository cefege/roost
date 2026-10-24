//! One reasoning block with a stable, accessible disclosure.
//! The transcript owns its identity; this component owns the reader's choice
//! so stream revisions cannot reopen or close a block unexpectedly.

use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Collapsible, CollapsibleContent, CollapsibleTrigger, Icon,
    IconType, Markdown, Shimmer,
};

#[component]
pub(super) fn ThinkingBlock(text: String, live: bool, globally_visible: bool) -> Element {
    let mut expanded = use_signal(|| false);
    let mut manually_changed = use_signal(|| false);
    let started_ms = use_hook(now_ms);
    let mut observed_seconds = use_signal(|| 0_u64);
    let mut shimmering = use_signal(|| live);
    use_effect(use_reactive((&live,), move |(live,)| shimmering.set(live)));
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
    let (copy_thought, thought_copied) =
        crate::components::hooks::use_copy_clipboard::use_copy_clipboard(None);
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
            section { class: "flex flex-col gap-1 text-sm", "data-live": live.then_some("true"),
                Collapsible {
                    class: "flex flex-col gap-1",
                    open: expanded() && globally_visible,
                    on_open_change: move |next: bool| {
                        manually_changed.set(true);
                        expanded.set(next);
                    },
                    CollapsibleTrigger {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        class: "justify-start text-muted-foreground",
                        disabled: !globally_visible,
                        Icon { icon: IconType::Brain, class: "size-4" }
                        if live {
                            Shimmer { loading: shimmering, "{label}" }
                        } else {
                            span { "{label}" }
                        }
                        Icon {
                            icon: if expanded() && globally_visible { IconType::ChevronUp } else { IconType::ChevronDown },
                            class: "size-4",
                        }
                    }
                    CollapsibleContent { class: "flex flex-col gap-2 border-l border-border pl-3 text-muted-foreground",
                        if omitted_live_lines {
                            p { class: "text-xs text-muted-foreground", "Earlier lines are omitted from the live preview." }
                        }
                        Markdown { class: "text-muted-foreground", html: super::markdown::markdown_to_safe_html(&tail) }
                        Button {
                            variant: ButtonVariant::Link,
                            class: "self-start",
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
