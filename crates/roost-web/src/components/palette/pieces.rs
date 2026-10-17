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

use crate::components::ui::{BindingChip, CommandItem};

/// The badge's stroke and text, one colour per kind.
const fn kind_classes(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Session => "border-info text-info",
        ItemKind::Workspace => "border-success text-success",
        ItemKind::Action => "border-muted-foreground text-muted-foreground",
    }
}

/// The kind of a row, in caps.
#[component]
pub fn KindBadge(kind: ItemKind) -> Element {
    let classes = kind_classes(kind);
    rsx! {
        span { class: "inline-flex shrink-0 items-center rounded-md border px-1 text-xs tracking-wider whitespace-nowrap uppercase {classes}",
            {kind.as_str()}
        }
    }
}

/// The row's own styling. The highlight comes from `aria-selected`, which
/// `CommandItem` sets from its `selected` prop, so no active/idle class pair.
const ROW_CLASS: &str = "h-auto w-full justify-between gap-3 rounded-none px-4 py-2 text-start font-normal whitespace-normal text-foreground aria-selected:bg-accent aria-selected:text-accent-foreground";

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
        div {
            class: "flex items-center justify-between gap-3 border-t border-border bg-card px-4 py-2 text-xs text-muted-foreground",
            "data-testid": "command-palette-footer",
            span { if has_results { "Select to open" } else { "Type to search" } }
            div { class: "flex items-center gap-3",
                FooterEntry { caps: vec!["↑", "↓"], label: "navigate" }
                FooterEntry { caps: vec!["↵"], label: "open" }
                FooterEntry { caps: vec!["esc"], label: "close" }
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
