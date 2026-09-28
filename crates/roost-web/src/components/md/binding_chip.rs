//! `BindingChip`: the one key-cap rendering for an input binding, a `<kbd>` in
//! the label-small mono ramp. Ported from
//! `apps/web/src/components/Settings/md/BindingChip.tsx`; the help overlay's
//! shortcut catalogue and the controller legend compose it. Tokens only.

use dioxus::prelude::*;

/// The key cap's inline style, every value a token.
pub const BINDING_CHIP_STYLE: &str = "font: var(--md-label-s-weight) var(--md-label-s-size)/var(--md-label-s-line) var(--font-mono); \
     padding: var(--md-space-1) var(--md-space-2); \
     border-radius: var(--md-shape-xs); \
     background: var(--surface-1); \
     border: var(--workbench-border-width) solid var(--md-sys-color-outline-variant); \
     color: var(--text-mid); \
     white-space: nowrap;";

/// One key cap.
#[component]
pub fn BindingChip(children: Element) -> Element {
    rsx! {
        kbd { style: BINDING_CHIP_STYLE, {children} }
    }
}
