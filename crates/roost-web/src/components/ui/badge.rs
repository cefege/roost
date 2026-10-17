//! `Badge`: a small status or count label, with the rust-ui variant and size
//! recipes. `Chip` and `BindingChip` build their class on `badge_class`, so every
//! label in the app shares one recipe. Depends on nothing else in `ui`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadgeVariant {
    #[default]
    Default,
    Secondary,
    Accent,
    Muted,
    Destructive,
    Outline,
    Success,
    Warning,
    Info,
}

impl BadgeVariant {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "border-transparent shadow bg-primary text-primary-foreground hover:bg-primary/80",
            Self::Secondary => "border-transparent bg-secondary text-secondary-foreground hover:bg-secondary/80",
            Self::Accent => "border-transparent bg-accent text-accent-foreground hover:bg-accent/80",
            Self::Muted => "border-transparent bg-muted text-muted-foreground hover:bg-muted/80",
            Self::Destructive => {
                "border-transparent shadow bg-destructive text-destructive-foreground hover:bg-destructive/80"
            }
            Self::Outline => "text-foreground",
            Self::Success => "border-transparent bg-success/15 text-success hover:bg-success/25",
            Self::Warning => "border-transparent bg-warning/15 text-warning hover:bg-warning/25",
            Self::Info => "border-transparent bg-info/15 text-info hover:bg-info/25",
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BadgeSize {
    #[default]
    Default,
    Sm,
    Lg,
}

impl BadgeSize {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "px-2.5 py-0.5 text-xs",
            Self::Sm => "px-1.5 py-0.5 text-xs",
            Self::Lg => "px-3 py-1 text-sm",
        }
    }
}

/// The badge's class attribute: the rust-ui base, then variant, size and the
/// caller's class, with later utilities winning conflicts.
pub fn badge_class(variant: BadgeVariant, size: BadgeSize, class: Option<&str>) -> String {
    tw_merge!(
        "inline-flex items-center font-semibold rounded-md border transition-colors focus:outline-hidden focus:ring-2 focus:ring-ring focus:ring-offset-2 w-fit",
        variant.as_str(),
        size.as_str(),
        class.unwrap_or("")
    )
}

#[derive(Props, Clone, PartialEq, Debug)]
pub struct BadgeProps {
    #[props(default)]
    pub variant: BadgeVariant,
    #[props(default)]
    pub size: BadgeSize,
    pub class: Option<String>,
    pub children: Element,
}

/// A status or count label.
#[component]
pub fn Badge(props: BadgeProps) -> Element {
    let merged = badge_class(props.variant, props.size, props.class.as_deref());
    rsx! {
        span { class: "{merged}", {props.children} }
    }
}
