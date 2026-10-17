//! `ToggleGroup`: a row or column of toggle buttons sharing one border and
//! radius. Spacing is a closed set, so the gap is a Tailwind class and the
//! grouped (flush) layout rounds only the outer corners.
//!
//! Each `ToggleGroupItem` is a button with `aria-pressed` and reports clicks to
//! its caller, who owns the pressed state.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToggleGroupVariant {
    #[default]
    Default,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToggleGroupOrientation {
    #[default]
    Horizontal,
    Vertical,
}

/// The gap between items. `Flush` joins them into one control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToggleGroupSpacing {
    #[default]
    Snug,
    Flush,
    Loose,
}

impl ToggleGroupSpacing {
    const fn gap_class(self) -> &'static str {
        match self {
            Self::Snug => "gap-1",
            Self::Flush => "gap-0",
            Self::Loose => "gap-2",
        }
    }

    const fn data_value(self) -> &'static str {
        match self {
            Self::Snug => "snug",
            Self::Flush => "flush",
            Self::Loose => "loose",
        }
    }
}

#[derive(Clone, Copy)]
struct ToggleGroupCtx {
    variant: ToggleGroupVariant,
    orientation: ToggleGroupOrientation,
    spacing: ToggleGroupSpacing,
}

#[component]
pub fn ToggleGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ToggleGroupVariant::Default)] variant: ToggleGroupVariant,
    #[props(default = ToggleGroupOrientation::Horizontal)] orientation: ToggleGroupOrientation,
    #[props(default = ToggleGroupSpacing::Snug)] spacing: ToggleGroupSpacing,
    children: Element,
) -> Element {
    provide_context(ToggleGroupCtx {
        variant,
        orientation,
        spacing,
    });

    let is_vertical = orientation == ToggleGroupOrientation::Vertical;
    let is_outline = variant == ToggleGroupVariant::Outline;
    let merged = tw_merge!(
        "flex items-center rounded-md group/toggle-group w-fit",
        spacing.gap_class(),
        if is_vertical { "flex-col" } else { "" },
        if is_outline { "shadow-xs" } else { "" },
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            class: "{merged}",
            role: "group",
            "data-name": "ToggleGroup",
            "data-variant": if is_outline { "Outline" } else { "Default" },
            "data-orientation": if is_vertical { "Vertical" } else { "Horizontal" },
            "data-spacing": spacing.data_value(),
            {children}
        }
    }
}

#[component]
pub fn ToggleGroupAction(
    #[props(into, optional)] class: Option<String>,
    #[props(into)] href: String,
    #[props(into, optional)] target: Option<String>,
    #[props(into, optional)] title: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "inline-flex gap-2 justify-center items-center p-0 text-sm font-medium whitespace-nowrap rounded-sm transition-all outline-none disabled:opacity-50 disabled:pointer-events-none [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0 [&_svg]:shrink-0 aria-invalid:ring-destructive/20 aria-invalid:border-destructive size-6 dark:aria-invalid:ring-destructive/40 dark:hover:bg-accent/50 hover:bg-accent hover:text-accent-foreground focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3",
        class.as_deref().unwrap_or("")
    );

    rsx! {
        a {
            class: "{merged}",
            href: "{href}",
            target: target.as_deref(),
            title: title.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn ToggleGroupItem(
    #[props(into, optional)] class: Option<String>,
    #[props(into)] title: String,
    #[props(default = false)] pressed: bool,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let ctx = use_context::<ToggleGroupCtx>();
    let is_vertical = ctx.orientation == ToggleGroupOrientation::Vertical;
    let is_flush = ctx.spacing == ToggleGroupSpacing::Flush;
    let is_outline = ctx.variant == ToggleGroupVariant::Outline;

    let rounded = match (is_flush, is_vertical) {
        (true, true) => "rounded-none first:rounded-t-md last:rounded-b-md",
        (true, false) => "rounded-none first:rounded-l-md last:rounded-r-md",
        (false, _) => "rounded-md",
    };
    let border = match (is_outline, is_flush, is_vertical) {
        (true, true, true) => "border border-t-0 first:border-t",
        (true, true, false) => "border border-l-0 first:border-l",
        (true, false, _) => "border",
        (false, _, _) => "",
    };
    let width = if is_vertical { "w-full" } else { "" };

    let merged = tw_merge!(
        "inline-flex flex-1 gap-2 justify-center items-center px-2 min-w-0 h-9 text-sm font-medium whitespace-nowrap bg-transparent shadow-none outline-none focus:z-10 focus-visible:z-10 disabled:opacity-50 disabled:pointer-events-none data-[state=on]:bg-accent data-[state=on]:text-accent-foreground [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0 transition-[color,box-shadow] aria-invalid:ring-destructive/20 aria-invalid:border-destructive shrink-0 dark:aria-invalid:ring-destructive/40 hover:bg-muted hover:text-muted-foreground focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3",
        rounded,
        border,
        width,
        class.as_deref().unwrap_or("")
    );

    let state = if pressed { "on" } else { "off" };
    rsx! {
        button {
            r#type: "button",
            "data-name": "ToggleGroupItem",
            class: "{merged}",
            title: "{title}",
            "aria-pressed": "{pressed}",
            "data-state": "{state}",
            onclick: move |event| {
                if let Some(handler) = &onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}
