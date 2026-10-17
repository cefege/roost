//! `Surface`: the single owner of a panel's background, radius, border and
//! elevation, every value a theme token. Banded regions, notices and gallery
//! sections compose it instead of a styled `<div>`. Callers keep only their
//! content layout, which arrives in `style` and is appended after the surface.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The element the surface renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceElement {
    #[default]
    Div,
    /// For a landmark with its own heading.
    Section,
}

/// The corner radius, from the `--md-shape-*` scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceRadius {
    /// Square corners, for full-bleed stacked bands.
    None,
    Xs,
    Sm,
    #[default]
    Md,
    Lg,
    Xl,
    Full,
}

impl SurfaceRadius {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "rounded-none",
            Self::Xs => "[border-radius:var(--md-shape-xs)]",
            Self::Sm => "[border-radius:var(--md-shape-sm)]",
            Self::Md => "[border-radius:var(--md-shape-md)]",
            Self::Lg => "[border-radius:var(--md-shape-lg)]",
            Self::Xl => "[border-radius:var(--md-shape-xl)]",
            Self::Full => "[border-radius:var(--md-shape-full)]",
        }
    }
}

/// Out-of-scale steps are clamped onto the scale: a `--surface-7` that names no
/// token would paint a transparent panel that looks like a missing surface.
const MAX_LEVEL: u8 = 3;
const MAX_ELEVATION: u8 = 5;
const MIN_PAD: u8 = 1;
const MAX_PAD: u8 = 9;

fn level_class(level: u8) -> &'static str {
    match level {
        0 => "bg-[var(--surface-0)]",
        1 => "bg-[var(--surface-1)]",
        2 => "bg-[var(--surface-2)]",
        _ => "bg-[var(--surface-3)]",
    }
}

fn elevation_class(elevation: u8) -> &'static str {
    match elevation {
        0 => "[box-shadow:var(--md-elev-0)]",
        1 => "[box-shadow:var(--md-elev-1)]",
        2 => "[box-shadow:var(--md-elev-2)]",
        3 => "[box-shadow:var(--md-elev-3)]",
        4 => "[box-shadow:var(--md-elev-4)]",
        _ => "[box-shadow:var(--md-elev-5)]",
    }
}

fn pad_class(pad: u8) -> &'static str {
    match pad {
        1 => "[padding:var(--md-space-1)]",
        2 => "[padding:var(--md-space-2)]",
        3 => "[padding:var(--md-space-3)]",
        4 => "[padding:var(--md-space-4)]",
        5 => "[padding:var(--md-space-5)]",
        6 => "[padding:var(--md-space-6)]",
        7 => "[padding:var(--md-space-7)]",
        8 => "[padding:var(--md-space-8)]",
        _ => "[padding:var(--md-space-9)]",
    }
}

/// The surface's class attribute: background level, elevation, radius, optional
/// padding and border, then the caller's class, with later utilities winning.
pub fn surface_classes(
    level: u8,
    elevation: u8,
    radius: SurfaceRadius,
    pad: Option<u8>,
    border: bool,
    class: Option<&str>,
) -> String {
    let pad_class_name = pad.map_or("", |pad| pad_class(pad.clamp(MIN_PAD, MAX_PAD)));
    let border_class = if border {
        "border border-[var(--md-outline-variant)]"
    } else {
        ""
    };
    tw_merge!(
        level_class(level.min(MAX_LEVEL)),
        elevation_class(elevation.min(MAX_ELEVATION)),
        radius.as_str(),
        pad_class_name,
        border_class,
        class.unwrap_or("")
    )
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
    let class = surface_classes(level, elevation, radius, pad, border, class.as_deref());
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
                class: "{class}",
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
                class: "{class}",
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
