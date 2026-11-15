//! `ToastSpecimen`: every `ToastPresentation` variant with its title-row
//! actions, then a three-card stack that collapses and fans out the way the
//! notification dock does. Rendered from the shipped component and the dock's
//! own stack recipes; local signals own the dismissals and the fan state.

use dioxus::prelude::*;

use crate::components::deck::deck_dom;
use crate::components::layout::window_size::use_is_compact;
use crate::components::notifications::toast_stack::{
    CARDS_COLLAPSED, CARDS_FANNED, stacked_item_class,
};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, ToastPresentation, ToastType};

const COLUMN_CLASS: &str = "grid gap-3 w-[min(var(--roost-toast-inline-size),100%)]";
const STACK_CLASS: &str = "w-[min(var(--roost-toast-inline-size),100%)] pt-10";

const VARIANTS: [ToastType; 6] = [
    ToastType::Default,
    ToastType::Success,
    ToastType::Error,
    ToastType::Warning,
    ToastType::Info,
    ToastType::Loading,
];

/// The three cards of the stack demo, oldest first.
const STACK_CARDS: [(ToastType, &str); 3] = [
    (ToastType::Info, "Roost just updated"),
    (ToastType::Warning, "Transfer stalled"),
    (ToastType::Success, "Upload finished"),
];

fn sample_title(variant: ToastType) -> &'static str {
    match variant {
        ToastType::Default => "Notification",
        ToastType::Success => "Upload finished",
        ToastType::Error => "Build failed",
        ToastType::Warning => "Transfer stalled",
        ToastType::Info => "Roost just updated",
        ToastType::Loading => "Sending shot.png",
    }
}

fn sample_description(variant: ToastType) -> Option<&'static str> {
    match variant {
        ToastType::Error => Some("exit 2 · 14s"),
        ToastType::Warning => Some("No progress for 15 s"),
        ToastType::Info => Some("Reload when convenient."),
        ToastType::Loading => Some("12 MB / 20 MB · 60%"),
        ToastType::Default | ToastType::Success => None,
    }
}

fn sample_actions(variant: ToastType) -> Option<Element> {
    match variant {
        ToastType::Success => Some(rsx! {
            Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm, "View" }
        }),
        ToastType::Error => Some(rsx! {
            Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm, "Copy" }
        }),
        ToastType::Info => Some(rsx! {
            Button { variant: ButtonVariant::Default, size: ButtonSize::Sm, "Reload now" }
        }),
        ToastType::Default | ToastType::Warning | ToastType::Loading => None,
    }
}

/// The variant column, then the collapsed stack.
#[component]
pub fn ToastSpecimen() -> Element {
    let mut dismissed = use_signal(Vec::<ToastType>::new);
    let mut fanned = use_signal(|| false);
    // The dock's rule: a tap synthesizes `mouseenter` with no `mouseleave`, so
    // a touch pointer taps to fan instead of hovering.
    let hoverable = !use_is_compact() && !deck_dom::is_touch_device();
    rsx! {
        div { class: "flex flex-col gap-6",
            div { class: COLUMN_CLASS, "data-testid": "toast-specimen-variants",
                for variant in VARIANTS {
                    if !dismissed.read().contains(&variant) {
                        ToastPresentation {
                            key: "{variant.as_str()}",
                            variant,
                            title: sample_title(variant),
                            description: sample_description(variant).map(str::to_owned),
                            actions: sample_actions(variant),
                            progress: match variant {
                                ToastType::Loading => None,
                                ToastType::Info => Some(0.6),
                                _ => Some(1.0),
                            },
                            on_close: (variant != ToastType::Loading).then(|| {
                                EventHandler::new(move |_event: MouseEvent| {
                                    dismissed.write().push(variant);
                                })
                            }),
                        }
                    }
                }
                div { class: "flex",
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        onclick: move |_event: MouseEvent| dismissed.write().clear(),
                        "Reset"
                    }
                }
            }
            div { class: STACK_CLASS,
                div {
                    class: if fanned() { CARDS_FANNED } else { CARDS_COLLAPSED },
                    "data-testid": "toast-specimen-stack",
                    "data-fanned": if fanned() { "true" } else { "false" },
                    onmouseenter: move |_event: MouseEvent| {
                        if hoverable {
                            fanned.set(true);
                        }
                    },
                    onmouseleave: move |_event: MouseEvent| {
                        if hoverable {
                            fanned.set(false);
                        }
                    },
                    onfocusin: move |_event: FocusEvent| {
                        if hoverable {
                            fanned.set(true);
                        }
                    },
                    onfocusout: move |_event: FocusEvent| {
                        if hoverable {
                            fanned.set(false);
                        }
                    },
                    onclick: move |_event: MouseEvent| {
                        if !hoverable {
                            fanned.toggle();
                        }
                    },
                    for (position, (variant, title)) in STACK_CARDS.into_iter().enumerate() {
                        div {
                            key: "{position}",
                            class: stacked_item_class(STACK_CARDS.len() - 1 - position, fanned()),
                            ToastPresentation { variant, title }
                        }
                    }
                }
            }
        }
    }
}
