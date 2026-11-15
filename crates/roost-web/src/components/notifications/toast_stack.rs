//! The dock's one stack of cards: toasts, undo cards, transfers and the
//! newer-build notice, oldest to front. Collapsed, the front card is full size
//! and up to two older cards peek behind it at rust-ui's offsets; hovering,
//! focusing or tapping the stack fans every card out. Cards past the window
//! fold into an overflow row that expands or clears them. Toast identity and
//! deadlines are `roost_client_core::store::toasts`; the ordering is
//! `dock_entries`.

use dioxus::prelude::*;
use roost_client_core::store::toasts::dismiss_all_toasts;
use roost_client_core::store::transfers::remove_transfer;

use super::dock_entries::{DockEntry, dock_entries};
use super::inline_transfer::use_inline_transfer_presentation;
use super::notify_target::NotifyTarget;
use super::store_write::write_store;
use super::toast_card::ToastCard;
use super::transfer_toast::TransferToast;
use super::undo_banner::UndoCloseToast;
use super::version_banner::{VersionToast, use_stale_build};
use crate::components::deck::deck_dom;
use crate::components::layout::window_size::use_is_compact;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, StatusDot};
use crate::pump::use_store;

/// Cards the stack shows before the rest fold into the overflow row: the front
/// card and the two that peek behind it.
pub const VISIBLE_DOCK_CARDS: usize = 3;

/// Collapsed: every card shares one grid cell, so later DOM order paints on top.
pub const CARDS_COLLAPSED: &str = "grid";

/// Fanned: plain flow, each card at its own height.
pub const CARDS_FANNED: &str = "flex flex-col gap-2";

/// How many of `live` cards, oldest first, fold into the overflow row.
#[must_use]
pub fn folded_toast_count(live: usize, expanded: bool) -> usize {
    if expanded {
        return 0;
    }
    live.saturating_sub(VISIBLE_DOCK_CARDS)
}

/// The wrapper class of the card `stack_index` places behind the front card
/// (0 is the front). Collapsed cards share one grid cell and `self-end` keeps
/// the front card's bottom edge on the dock's edge; translate and scale are
/// their own properties, so they compose with the card's entrance animation.
#[must_use]
pub const fn stacked_item_class(stack_index: usize, fanned: bool) -> &'static str {
    if fanned {
        return "min-w-0 transition-transform duration-300 ease-out motion-reduce:transition-none";
    }
    match stack_index {
        0 => {
            "min-w-0 [grid-area:1/1] self-end transition-transform duration-300 ease-out motion-reduce:transition-none"
        }
        1 => {
            "min-w-0 [grid-area:1/1] self-end -translate-y-5 scale-95 transition-transform duration-300 ease-out motion-reduce:transition-none"
        }
        _ => {
            "min-w-0 [grid-area:1/1] self-end -translate-y-10 scale-90 transition-transform duration-300 ease-out motion-reduce:transition-none"
        }
    }
}

/// Every live card, oldest first, with the oldest folded behind the overflow
/// row once the stack outgrows its window.
#[component]
pub fn ToastStack() -> Element {
    let pump = use_store();
    let compact = use_is_compact();
    let stale = use_stale_build();
    let inline = use_inline_transfer_presentation();
    let mut expanded = use_signal(|| false);
    let mut fanned = use_signal(|| false);
    let mut notify_target = use_context::<NotifyTarget>();
    let entries = {
        let core = pump.core();
        let core = core.borrow();
        dock_entries(core.store(), inline, stale)
    };
    let overflowing = folded_toast_count(entries.len(), false) > 0;
    let stackable = entries.len() > 1;
    // A burst that drained back inside the window ends the expansion, so the
    // next burst starts folded instead of inheriting a stale "show all".
    use_effect(use_reactive((&overflowing,), move |(overflowing,)| {
        if !overflowing && *expanded.peek() {
            expanded.set(false);
        }
    }));
    // A lone card has nothing to fan, so the next stack starts collapsed.
    use_effect(use_reactive((&stackable,), move |(stackable,)| {
        if !stackable && *fanned.peek() {
            fanned.set(false);
        }
    }));
    if entries.is_empty() {
        return rsx! {};
    }
    let is_expanded = expanded() && overflowing;
    let folded = folded_toast_count(entries.len(), is_expanded);
    let folded_error = entries[..folded].iter().any(DockEntry::is_error);
    let shown_count = entries.len() - folded;
    let fanned_now = is_expanded || (fanned() && shown_count > 1);
    // A tap on a touch device synthesizes `mouseenter` with no `mouseleave`,
    // which would pin the fan open. Width alone does not catch a touch laptop,
    // so this asks the pointer.
    let hoverable = !compact && !deck_dom::is_touch_device();
    let toast_ids = entries
        .iter()
        .filter_map(|entry| match entry {
            DockEntry::Toast { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let settled_transfers = entries
        .iter()
        .filter_map(|entry| match entry {
            DockEntry::Transfer(transfer) if transfer.state.is_terminal() => {
                Some(transfer.id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let total = entries.len();
    let cards = entries
        .into_iter()
        .skip(folded)
        .enumerate()
        .map(|(position, entry)| (entry.key(), shown_count - 1 - position, entry))
        .collect::<Vec<_>>();
    let clear_pump = pump.clone();
    let clear_all = move |_event: MouseEvent| {
        for id in &toast_ids {
            notify_target.clear(id);
        }
        write_store(&clear_pump, |store| {
            dismiss_all_toasts(store);
            for id in &settled_transfers {
                remove_transfer(store, id);
            }
        });
        tracing::info!(
            target: "notifications",
            toasts = toast_ids.len(),
            transfers = settled_transfers.len(),
            "toast stack cleared"
        );
    };
    let label = if is_expanded {
        format!("{total} notifications")
    } else if folded == 1 {
        "1 more notification".to_owned()
    } else {
        format!("{folded} more notifications")
    };

    rsx! {
        div {
            class: "flex flex-col gap-2 data-[expanded=true]:[max-block-size:calc(100dvh_-_var(--roost-notify-dock-lift)_-_6rem)] data-[expanded=true]:overflow-y-auto data-[expanded=true]:overscroll-contain",
            role: "region",
            "aria-label": "Notifications",
            "data-testid": "toast-stack",
            "data-expanded": if is_expanded { "true" } else { "false" },
            if overflowing {
                div {
                    class: "flex items-center gap-2 px-1 text-xs text-muted-foreground",
                    "data-testid": "toast-overflow",
                    if folded_error {
                        StatusDot { status: "error".to_owned() }
                    }
                    span { class: "min-w-0 flex-1 font-medium", "{label}" }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        "data-testid": "toast-overflow-toggle",
                        "aria-expanded": if is_expanded { "true" } else { "false" },
                        onclick: move |_event: MouseEvent| expanded.toggle(),
                        if is_expanded { "Show less" } else { "Show all" }
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        "data-testid": "toast-clear-all",
                        onclick: clear_all,
                        "Clear all"
                    }
                }
            }
            div {
                class: if fanned_now { CARDS_FANNED } else { CARDS_COLLAPSED },
                "data-fanned": if fanned_now { "true" } else { "false" },
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
                for (key, stack_index, entry) in cards {
                    div {
                        key: "{key}",
                        "data-stack-index": "{stack_index}",
                        class: stacked_item_class(stack_index, fanned_now),
                        {dock_card(entry)}
                    }
                }
            }
        }
    }
}

/// The card one entry renders as.
fn dock_card(entry: DockEntry) -> Element {
    match entry {
        DockEntry::Version(stale) => rsx! { VersionToast { stale } },
        DockEntry::Transfer(transfer) => rsx! { TransferToast { transfer } },
        DockEntry::Toast { id, .. } => rsx! { ToastCard { toast_id: id } },
        DockEntry::Undo(entry) => rsx! { UndoCloseToast { entry } },
    }
}

#[cfg(test)]
mod tests {
    use super::{CARDS_COLLAPSED, CARDS_FANNED, folded_toast_count, stacked_item_class};

    #[test]
    fn only_the_cards_past_the_window_fold_and_expanding_unfolds_them() {
        assert_eq!(
            folded_toast_count(3, false),
            0,
            "a full window folds nothing"
        );
        assert_eq!(folded_toast_count(5, false), 2, "the two oldest fold");
        assert_eq!(
            folded_toast_count(2, false),
            0,
            "a phone collapses up to three cards too"
        );
        assert_eq!(folded_toast_count(5, true), 0, "expanded shows every card");
    }

    #[test]
    fn a_collapsed_stack_offsets_cards_behind_the_front_and_a_fanned_one_does_not() {
        let front = stacked_item_class(0, false);
        assert!(!front.contains("translate") && !front.contains("scale"));
        let second = stacked_item_class(1, false);
        assert!(second.contains("-translate-y-5") && second.contains("scale-95"));
        let third = stacked_item_class(2, false);
        assert!(third.contains("-translate-y-10") && third.contains("scale-90"));
        for index in 0..3 {
            let fanned = stacked_item_class(index, true);
            assert!(!fanned.contains("translate") && !fanned.contains("grid-area"));
        }
        assert_eq!(CARDS_COLLAPSED, "grid");
        assert!(CARDS_FANNED.contains("flex-col"));
    }
}
