//! Presentational pieces of the command palette: the kind badge, one result row
//! and the keyboard-legend footer. Called by `palette::body` only; depends on
//! `md` primitives and `roost_client_core::store::palette`. Ports
//! `CommandPalettePieces.tsx` and the row markup of `CommandPaletteBody.tsx`.
//!
//! Every value here is a token, including the badge's 10px/14px ramp: the
//! design ratchet rejects a raw value, and `--md-label-s-*` is the ramp the rest
//! of the chrome already uses for a caps label.

use dioxus::prelude::*;
use roost_client_core::store::palette::{ItemKind, PaletteItem};

use crate::components::md::{BindingChip, Button, ButtonVariant};

/// The badge's stroke and text, one colour per kind.
const fn kind_color(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Session => "var(--status-info)",
        ItemKind::Workspace => "var(--status-ok)",
        ItemKind::Action => "var(--text-lo)",
    }
}

const BADGE_STYLE: &str = "background: transparent; border: 1px solid; border-radius: var(--md-shape-xs); \
     padding: 0 var(--md-space-1); flex-shrink: 0; letter-spacing: 0.04em; \
     font: var(--md-label-s-weight) var(--md-label-s-size) / var(--md-label-s-line) var(--md-font); \
     text-transform: uppercase; white-space: nowrap;";

/// The kind of a row, in caps.
#[component]
pub fn KindBadge(kind: ItemKind) -> Element {
    let color = kind_color(kind);
    rsx! {
        span { style: "{BADGE_STYLE} border-color: {color}; color: {color};", {kind.as_str()} }
    }
}

/// The shared shape of a result row; only the colours differ.
const ROW_STYLE: &str = "inline-size: 100%; display: flex; align-items: center; justify-content: space-between; \
     gap: var(--md-space-3); padding: var(--md-space-2) var(--md-space-4); border: none; text-align: start; \
     font: var(--md-body-m-weight) var(--md-body-m-size) / var(--md-body-m-line) var(--md-font);";
const ROW_IDLE_STYLE: &str = "background: transparent; color: var(--md-on-surface);";
const ROW_ACTIVE_STYLE: &str =
    "background: var(--md-secondary-container); color: var(--md-on-secondary-container);";
const ROW_LABEL_STYLE: &str = "overflow: hidden; text-overflow: ellipsis; white-space: nowrap;";
const ROW_HINT_STYLE: &str = "color: var(--text-lo); flex-shrink: 0; overflow: hidden; \
     text-overflow: ellipsis; white-space: nowrap; font: var(--md-label-s-weight) var(--md-label-s-size) / var(--md-label-s-line) var(--md-font);";

/// One result: what it is, what it is called, and the dimmer hint beside it.
#[component]
pub fn PaletteRow(
    item: PaletteItem,
    index: usize,
    active: bool,
    on_select: EventHandler<PaletteItem>,
) -> Element {
    let state = if active {
        ROW_ACTIVE_STYLE
    } else {
        ROW_IDLE_STYLE
    };
    let hint = item.hint.clone();
    let label = item.label.clone();
    let kind = item.kind;
    let selected = item.clone();
    rsx! {
        Button {
            variant: ButtonVariant::Ghost,
            style: "{ROW_STYLE} {state}",
            onclick: move |_| on_select.call(selected.clone()),
            // `data-index` is what a host reads to follow the pointer with the
            // cursor: `md::Button` forwards native attributes but declares no
            // `onmouseenter`, so the row cannot carry the listener itself.
            "data-index": index.to_string(),
            "data-testid": "command-palette-item",
            "data-kind": kind.as_str(),
            span { style: "display: flex; align-items: center; gap: var(--md-space-2); min-inline-size: 0;",
                KindBadge { kind }
                span { style: ROW_LABEL_STYLE, {label} }
            }
            if let Some(hint) = hint {
                span { style: ROW_HINT_STYLE, {hint} }
            }
        }
    }
}

const FOOTER_STYLE: &str = "display: flex; align-items: center; justify-content: space-between; \
     gap: var(--md-space-3); padding: var(--md-space-2) var(--md-space-4); \
     border-block-start: var(--workbench-border-width) solid var(--md-sys-color-outline-variant); \
     background: var(--md-sys-color-surface-container); \
     font: var(--md-label-s-weight) var(--md-label-s-size) / var(--md-label-s-line) var(--md-font); \
     color: var(--md-sys-color-on-surface-variant);";

/// The legend under the results: what the arrows do, and whether there is
/// anything to open yet.
#[component]
pub fn PaletteFooter(has_results: bool) -> Element {
    rsx! {
        div { "data-testid": "command-palette-footer", style: FOOTER_STYLE,
            span { if has_results { "Select to open" } else { "Type to search" } }
            div { style: "display: flex; align-items: center; gap: var(--md-space-3);",
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
        span { style: "display: flex; align-items: center; gap: var(--md-space-1);",
            for cap in caps {
                BindingChip { {cap} }
            }
            {label}
        }
    }
}
