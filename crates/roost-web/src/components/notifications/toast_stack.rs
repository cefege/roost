//! The dock's live toast cards, oldest first so the newest sits nearest the
//! dock's anchored edge, capped at a few visible cards with an overflow bar that
//! expands the backlog or clears it. The stack itself is
//! `roost_client_core::store::toasts::ToastStack`, whose identity rule makes
//! "the same event produced two cards" unrepresentable; this file walks it.

use dioxus::prelude::*;
use roost_client_core::store::toasts::{ToastId, ToastKind, dismiss_all_toasts};

use super::notify_target::NotifyTarget;
use super::store_write::write_store;
use super::toast_card::ToastCard;
use crate::components::layout::window_size::use_is_compact;
use crate::pump::use_store;
use libero::{
    components::{Button, Flex, Indicator, Paper, Text},
    sx::sx,
};

/// Cards a pointer layout shows before the rest fold into the overflow bar —
/// the corner-stack convention: enough to read a burst, never a wall over the
/// terminal.
pub const POINTER_VISIBLE_TOASTS: usize = 3;

/// Cards a phone shows: one, because the column spans the screen width above
/// the composer and every extra card takes a terminal row.
pub const COMPACT_VISIBLE_TOASTS: usize = 1;

/// How many of `live` cards, oldest first, fold into the overflow bar.
#[must_use]
pub fn folded_toast_count(live: usize, compact: bool, expanded: bool) -> usize {
    if expanded {
        return 0;
    }
    let visible = if compact {
        COMPACT_VISIBLE_TOASTS
    } else {
        POINTER_VISIBLE_TOASTS
    };
    live.saturating_sub(visible)
}

/// Every live card, oldest first, with the oldest folded behind the overflow
/// bar once the stack outgrows its window.
#[component]
pub fn ToastStack() -> Element {
    let pump = use_store();
    let compact = use_is_compact();
    let mut expanded = use_signal(|| false);
    let mut notify_target = use_context::<NotifyTarget>();
    let stack_sx = use_hook(|| {
        sx().selector(
            "&[data-expanded='true']",
            sx().max_height("calc(100dvh - var(--roost-notify-dock-lift) - 6rem)")
                .overflow_y("auto")
                .with("overscroll-behavior", "contain"),
        )
    });
    let overflow_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("xs")
            .padding("xs")
            .padding_left("sm")
    });
    let label_sx = use_hook(|| sx().min_width("0").flex("1"));
    let cards = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .toasts
            .toasts()
            .map(|toast| (toast.id.clone(), toast.raised_order, toast.kind))
            .collect::<Vec<_>>()
    };
    let overflowing = folded_toast_count(cards.len(), compact, false) > 0;
    // A burst that drained back inside the window ends the expansion, so the
    // next burst starts folded instead of inheriting a stale "show all".
    use_effect(use_reactive((&overflowing,), move |(overflowing,)| {
        if !overflowing && *expanded.peek() {
            expanded.set(false);
        }
    }));
    if cards.is_empty() {
        return rsx! {};
    }
    let is_expanded = expanded() && overflowing;
    let folded = folded_toast_count(cards.len(), compact, is_expanded);
    let folded_error = cards[..folded]
        .iter()
        .any(|(_, _, kind)| *kind == ToastKind::Err);
    let shown = cards[folded..].to_vec();
    let all_ids: Vec<ToastId> = cards.iter().map(|(id, _, _)| id.clone()).collect();
    let clear_pump = pump.clone();
    let clear_all = move |_event: MouseEvent| {
        for id in &all_ids {
            notify_target.clear(id);
        }
        write_store(&clear_pump, dismiss_all_toasts);
        tracing::info!(target: "notifications", cleared = all_ids.len(), "toast stack cleared");
    };
    let label = if is_expanded {
        format!("{} notifications", cards.len())
    } else if folded == 1 {
        "1 more notification".to_owned()
    } else {
        format!("{folded} more notifications")
    };

    rsx! {
        Flex {
            direction: "column", gap: "xs", sx: stack_sx,
            role: "region", aria_label: "Notifications",
            "data-testid": "toast-stack",
            "data-expanded": if is_expanded { "true" } else { "false" },
            if overflowing {
                Paper { shadow: "sm", radius: "md", bordered: true, sx: overflow_sx,
                    "data-testid": "toast-overflow",
                    if folded_error { Indicator { color: "error", aria_hidden: "true" } }
                    Text { size: "xs", color: "muted", sx: label_sx, "{label}" }
                    Button { variant: "standard", size: if compact { "sm" } else { "xs" },
                        "data-testid": "toast-overflow-toggle", aria_expanded: is_expanded,
                        onclick: move |_event: MouseEvent| expanded.toggle(),
                        if is_expanded { "Show less" } else { "Show all" }
                    }
                    Button { variant: "standard", size: if compact { "sm" } else { "xs" },
                        "data-testid": "toast-clear-all", onclick: clear_all, "Clear all"
                    }
                }
            }
            for (id, order, _) in shown { ToastCard { key: "{order}", toast_id: id } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::folded_toast_count;

    #[test]
    fn only_the_cards_past_the_window_fold_and_expanding_unfolds_them() {
        assert_eq!(
            folded_toast_count(3, false, false),
            0,
            "a full window folds nothing"
        );
        assert_eq!(
            folded_toast_count(5, false, false),
            2,
            "the two oldest fold"
        );
        assert_eq!(
            folded_toast_count(2, true, false),
            1,
            "a phone shows one card"
        );
        assert_eq!(
            folded_toast_count(5, false, true),
            0,
            "expanded shows every card"
        );
    }
}
