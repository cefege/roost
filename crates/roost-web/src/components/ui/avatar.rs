//! `Avatar` and its parts (`AvatarImage`, `AvatarFallback`, `AvatarBadge`,
//! `AvatarGroup`, `AvatarGroupCount`): rust-ui's avatar compound. Account and
//! machine identity compose it; a failed image falls back to the initials without
//! inline script, so the strict CSP holds.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum AvatarSize {
    Sm,
    #[default]
    Default,
    Lg,
}

impl AvatarSize {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sm => "sm",
            Self::Default => "default",
            Self::Lg => "lg",
        }
    }
}

#[component]
pub fn Avatar(
    #[props(default)] size: AvatarSize,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "group/avatar relative flex size-8 shrink-0 overflow-hidden rounded-full select-none after:absolute after:inset-0 after:rounded-full after:border after:border-border after:mix-blend-darken data-[size=lg]:size-10 data-[size=sm]:size-6 dark:after:mix-blend-lighten",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            class: "{merged}",
            "data-slot": "avatar",
            "data-size": size.as_str(),
            {children}
        }
    }
}

/// The image, shown until it fails to load. The failed source is remembered
/// rather than a bool, so a new `src` gets a fresh attempt.
#[component]
pub fn AvatarImage(
    #[props(into)] src: String,
    #[props(into, optional)] alt: Option<String>,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let mut failed_src = use_signal(|| None::<String>);
    let failed = failed_src.read().as_deref() == Some(src.as_str());
    let error_src = src.clone();
    let merged = tw_merge!(
        "absolute inset-0 aspect-square size-full z-10 rounded-full object-cover",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        if !failed {
            img {
                class: "{merged}",
                src: "{src}",
                alt: alt.as_deref().unwrap_or(""),
                "data-slot": "avatar-image",
                onerror: move |_| failed_src.set(Some(error_src.clone())),
            }
        }
    }
}

#[component]
pub fn AvatarFallback(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "absolute inset-0 flex size-full items-center justify-center rounded-full bg-muted text-sm text-muted-foreground group-data-[size=sm]/avatar:text-xs",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: "{merged}", {children} }
    }
}

#[component]
pub fn AvatarBadge(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "absolute right-0 bottom-0 z-10 inline-flex items-center justify-center rounded-full bg-primary text-primary-foreground ring-2 ring-background select-none group-data-[size=sm]/avatar:size-2 group-data-[size=sm]/avatar:[&>svg]:hidden group-data-[size=default]/avatar:size-2.5 group-data-[size=default]/avatar:[&>svg]:size-2 group-data-[size=lg]/avatar:size-3 group-data-[size=lg]/avatar:[&>svg]:size-2",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        span { class: "{merged}", {children} }
    }
}

#[component]
pub fn AvatarGroup(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "group/avatar-group flex -space-x-2 *:data-[slot=avatar]:ring-2 *:data-[slot=avatar]:ring-background",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: "{merged}", {children} }
    }
}

#[component]
pub fn AvatarGroupCount(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "relative flex size-8 shrink-0 items-center justify-center rounded-full bg-muted text-sm text-muted-foreground ring-2 ring-background group-has-data-[size=lg]/avatar-group:size-10 group-has-data-[size=sm]/avatar-group:size-6 [&>svg]:size-4 group-has-data-[size=lg]/avatar-group:[&>svg]:size-5 group-has-data-[size=sm]/avatar-group:[&>svg]:size-3",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: "{merged}", {children} }
    }
}
