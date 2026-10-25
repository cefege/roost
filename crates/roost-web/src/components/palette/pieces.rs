//! Presentational pieces of the command palette: the kind badge, one result row
//! and the keyboard-legend footer. Called by `palette::body` only; depends on
//! `ui` primitives and `roost_client_core::store::palette`. Ports
//! `CommandPalettePieces.tsx` and the row markup of `CommandPaletteBody.tsx`.
//!
//! Every colour is a theme role and every size a type-scale step, so the palette
//! reads like the rest of the chrome in both themes. Each class string is a
//! complete literal: Tailwind scans the source for them.

use dioxus::prelude::*;
use roost_client_core::store::palette::{ItemKind, PaletteItem};

use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, BindingChip, CommandFooter, CommandItem,
};

/// The badge's tone, one theme role per kind.
const fn kind_variant(kind: ItemKind) -> BadgeVariant {
    match kind {
        ItemKind::Session => BadgeVariant::Info,
        ItemKind::Workspace => BadgeVariant::Success,
        ItemKind::Action => BadgeVariant::Muted,
    }
}

/// The kind of a row, in caps.
#[component]
pub fn KindBadge(kind: ItemKind) -> Element {
    rsx! {
        Badge {
            variant: kind_variant(kind),
            size: BadgeSize::Sm,
            class: Some("shrink-0 whitespace-nowrap".to_owned()),
            {kind.as_str()}
        }
    }
}

/// The row's own styling. The highlight comes from `aria-selected`, which
/// `CommandItem` sets from its `selected` prop, so no active/idle class pair.
const ROW_CLASS: &str = "h-auto w-full justify-between text-start whitespace-normal";

/// One result: what it is, what it is called, and the dimmer hint beside it.
#[component]
pub fn PaletteRow(
    item: PaletteItem,
    index: usize,
    active: bool,
    on_select: EventHandler<PaletteItem>,
) -> Element {
    let hint = item.hint.clone();
    let label = item.label.clone();
    let kind = item.kind;
    let selected = item.clone();
    rsx! {
        CommandItem {
            class: ROW_CLASS.to_owned(),
            value: index.to_string(),
            selected: active,
            show_check: false,
            on_select: move |_: ()| on_select.call(selected.clone()),
            test_id: "command-palette-item",
            // `data-index` is what the host reads to follow the pointer with the
            // cursor; `CommandItem` forwards native attributes for it.
            "data-index": index.to_string(),
            "data-kind": kind.as_str(),
            span { class: "flex min-w-0 items-center gap-2",
                KindBadge { kind }
                span { class: "truncate", {label} }
            }
            if let Some(hint) = hint {
                span { class: "shrink-0 truncate text-xs text-muted-foreground", {hint} }
            }
        }
    }
}

/// The legend under the results: what the arrows do, and whether there is
/// anything to open yet.
#[component]
pub fn PaletteFooter(has_results: bool) -> Element {
    rsx! {
        div { "data-testid": "command-palette-footer",
            CommandFooter { class: "justify-between",
                span { if has_results { "Select to open" } else { "Type to search" } }
                div { class: "flex items-center gap-3",
                    FooterEntry { caps: vec!["↑", "↓"], label: "navigate" }
                    FooterEntry { caps: vec!["↵"], label: "open" }
                    FooterEntry { caps: vec!["esc"], label: "close" }
                }
            }
        }
    }
}

/// One key cap group with the verb it performs.
#[component]
fn FooterEntry(caps: Vec<&'static str>, label: &'static str) -> Element {
    rsx! {
        span { class: "flex items-center gap-1",
            for cap in caps {
                BindingChip { {cap} }
            }
            {label}
        }
    }
}
