//! `Surface`: THE panel — the single owner of a panel's background, shape,
//! border and elevation. Ported from
//! `apps/web/src/components/Settings/md/Surface.tsx`; every banded region,
//! notice and gallery section composes it instead of a styled `<div>`.
//!
//! Every visual value is a theme token; callers keep only their content layout,
//! which arrives in `style` and is appended after the surface's own declarations,
//! as v2 spread it last.

use dioxus::prelude::*;

/// The element the surface renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceElement {
    /// `<div>`.
    #[default]
    Div,
    /// `<section>`, for a landmark with its own heading.
    Section,
}

/// The corner radius, from the `--md-shape-*` scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceRadius {
    /// Square corners, for full-bleed stacked bands.
    None,
    /// `--md-shape-xs`.
    Xs,
    /// `--md-shape-sm`.
    Sm,
    /// `--md-shape-md`.
    #[default]
    Md,
    /// `--md-shape-lg`.
    Lg,
    /// `--md-shape-xl`.
    Xl,
    /// `--md-shape-full`.
    Full,
}

impl SurfaceRadius {
    /// The CSS value.
    pub const fn css_value(self) -> &'static str {
        match self {
            Self::None => "0",
            Self::Xs => "var(--md-shape-xs)",
            Self::Sm => "var(--md-shape-sm)",
            Self::Md => "var(--md-shape-md)",
            Self::Lg => "var(--md-shape-lg)",
            Self::Xl => "var(--md-shape-xl)",
            Self::Full => "var(--md-shape-full)",
        }
    }
}

/// The highest `--surface-N`, `--md-elev-N` and `--md-space-N` steps declared.
const MAX_LEVEL: u8 = 3;
const MAX_ELEVATION: u8 = 5;
const MIN_PAD: u8 = 1;
const MAX_PAD: u8 = 9;

/// The surface's inline style.
///
/// Out-of-scale steps are clamped onto the scale: v2's types made them
/// unrepresentable, and a `var(--surface-7)` that names no token would paint a
/// transparent panel that looks like a missing surface.
pub fn surface_style(
    level: u8,
    elevation: u8,
    radius: SurfaceRadius,
    pad: Option<u8>,
    border: bool,
    extra: Option<&str>,
) -> String {
    let mut style = format!(
        "background: var(--surface-{}); box-shadow: var(--md-elev-{}); border-radius: {};",
        level.min(MAX_LEVEL),
        elevation.min(MAX_ELEVATION),
        radius.css_value(),
    );
    if let Some(pad) = pad {
        style.push_str(&format!(
            " padding: var(--md-space-{});",
            pad.clamp(MIN_PAD, MAX_PAD)
        ));
    }
    if border {
        style.push_str(" border: 1px solid var(--md-outline-variant);");
    }
    if let Some(extra) = extra.map(str::trim).filter(|extra| !extra.is_empty()) {
        style.push(' ');
        style.push_str(extra);
    }
    style
}

/// A token-driven panel.
#[component]
pub fn Surface(
    #[props(default)] element: SurfaceElement,
    #[props(default = 1)] level: u8,
    #[props(default)] elevation: u8,
    #[props(default)] radius: SurfaceRadius,
    pad: Option<u8>,
    #[props(default)] border: bool,
    class: Option<String>,
    style: Option<String>,
    onmounted: Option<EventHandler<MountedEvent>>,
    onclick: Option<EventHandler<()>>,
    test_id: Option<String>,
    aria_labelledby: Option<String>,
    role: Option<String>,
    aria_live: Option<String>,
    aria_atomic: Option<String>,
    aria_hidden: Option<String>,
    children: Element,
) -> Element {
    let style = surface_style(level, elevation, radius, pad, border, style.as_deref());
    let on_mounted = move |event: MountedEvent| {
        if let Some(handler) = onmounted {
            handler.call(event);
        }
    };
    let on_click = move |_: MouseEvent| {
        if let Some(handler) = onclick {
            handler.call(());
        }
    };
    match element {
        SurfaceElement::Div => rsx! {
            div {
                class,
                style,
                role,
                "data-testid": test_id,
                "aria-labelledby": aria_labelledby,
                "aria-live": aria_live,
                "aria-atomic": aria_atomic,
                "aria-hidden": aria_hidden,
                onmounted: on_mounted,
                onclick: on_click,
                {children}
            }
        },
        SurfaceElement::Section => rsx! {
            section {
                class,
                style,
                role,
                "data-testid": test_id,
                "aria-labelledby": aria_labelledby,
                "aria-live": aria_live,
                "aria-atomic": aria_atomic,
                "aria-hidden": aria_hidden,
                onmounted: on_mounted,
                onclick: on_click,
                {children}
            }
        },
    }
}
