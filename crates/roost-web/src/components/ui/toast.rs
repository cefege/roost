//! Toast markup, matching rust-ui's `toast.rs` classes and `data-*` hooks.
//! Owns the enums and components; the queue and its timers live in
//! `toast_queue.rs`. Depends on `icon` for glyphs and `tw_merge` for class merging.

use dioxus::prelude::*;
use icons::IconType;
use tw_merge::tw_merge;

use super::icon::Icon;
use super::toast_queue::{ToastEntry, ToastHub};
pub use super::toast_queue::{ToastHandle, use_toast};

/// rust-ui's `VISIBLE_TOASTS_AMOUNT`: toasts this many from the front are visible.
const VISIBLE_TOASTS_AMOUNT: usize = 3;
/// rust-ui's `--max-toasts`: toasts beyond this many from the front are hidden.
const MAX_TOASTS: usize = 5;

/// The JS bridge's default CSS custom properties on each list.
const TOAST_CSS_VARS: &str = "--max-toasts: 5; --dismiss-delay: 5000ms; --enter-duration: 300ms; --exit-duration: 300ms; --stack-duration: 300ms; --stack-spacing: 20px; --expand-spacing: 110px; --gap: 15px; --scale-factor: 0.05; --transition-easing: ease-out; --stack-easing: ease-in-out;";

const TRIGGER_BASE: &str = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all disabled:pointer-events-none disabled:opacity-50 outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px] w-fit cursor-pointer h-9 px-4 py-2";
const LIST_CLASS: &str = "flex relative flex-col opacity-100 gap-[15px] h-[100px] w-[400px] pointer-events-none [&>*]:pointer-events-auto";
const ITEM_BASE: &str = "p-5 shadow-lg border rounded-lg cursor-grab absolute w-full max-w-96 touch-none flex items-start gap-3";
const ICON_CLASS: &str = "flex items-center justify-center w-5 h-5 shrink-0 mr-3";
const CLOSE_CLASS: &str = "w-5 h-5 flex items-center justify-center border-none bg-transparent text-muted-foreground cursor-pointer transition-colors duration-150 shrink-0 p-0 ml-auto hover:text-foreground";
const TRACK_CLASS: &str =
    "absolute bottom-0 inset-x-0 h-[3px] bg-black/10 overflow-hidden rounded-b-lg";
const PROGRESS_CLASS: &str = "h-full w-full bg-current opacity-30 origin-left";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastType {
    #[default]
    Default,
    Success,
    Error,
    Warning,
    Info,
    Loading,
}

impl ToastType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Success => "Success",
            Self::Error => "Error",
            Self::Warning => "Warning",
            Self::Info => "Info",
            Self::Loading => "Loading",
        }
    }

    const fn trigger_class(self) -> &'static str {
        match self {
            Self::Default => "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
            Self::Success => "bg-success text-success-foreground hover:bg-success/90",
            Self::Error => {
                "bg-destructive text-white shadow-xs hover:bg-destructive/90 dark:bg-destructive/60"
            }
            Self::Warning => "bg-warning text-warning-foreground hover:bg-warning/90",
            Self::Info => "bg-info text-info-foreground shadow-xs hover:bg-info/90",
            Self::Loading => {
                "bg-secondary text-secondary-foreground shadow-xs hover:bg-secondary/80"
            }
        }
    }

    const fn item_class(self) -> &'static str {
        match self {
            Self::Default | Self::Loading => "bg-background text-foreground border-border",
            Self::Success => "bg-success-light text-success-dark border-success",
            Self::Error => "bg-destructive-light text-destructive-dark border-destructive",
            Self::Warning => "bg-warning-light text-warning-dark border-warning",
            Self::Info => "bg-info-light text-info-dark border-info",
        }
    }

    const fn icon(self) -> Option<IconType> {
        match self {
            Self::Default => None,
            Self::Success => Some(IconType::CircleCheck),
            Self::Error => Some(IconType::CircleX),
            Self::Warning => Some(IconType::TriangleAlert),
            Self::Info => Some(IconType::Info),
            Self::Loading => Some(IconType::LoaderCircle),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastPosition {
    TopLeft,
    TopCenter,
    TopRight,
    #[default]
    BottomRight,
    BottomCenter,
    BottomLeft,
}

impl ToastPosition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "TopLeft",
            Self::TopCenter => "TopCenter",
            Self::TopRight => "TopRight",
            Self::BottomRight => "BottomRight",
            Self::BottomCenter => "BottomCenter",
            Self::BottomLeft => "BottomLeft",
        }
    }

    const fn container_class(self) -> &'static str {
        match self {
            Self::TopLeft => "left-6 top-6",
            Self::TopRight => "right-6 top-6",
            Self::TopCenter => "left-1/2 -translate-x-1/2 top-6",
            Self::BottomCenter => "left-1/2 -translate-x-1/2 bottom-6",
            Self::BottomLeft => "left-6 bottom-6",
            Self::BottomRight => "right-6 bottom-6",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastDirection {
    TopDown,
    #[default]
    BottomUp,
}

impl ToastDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TopDown => "TopDown",
            Self::BottomUp => "BottomUp",
        }
    }
}

/// Provides the queue to `ToastTrigger`, `ToastToaster` and `use_toast` below it.
/// Mount once above every trigger and toaster.
#[component]
pub fn Toaster(children: Element) -> Element {
    let hub = ToastHub::use_new();
    use_context_provider(|| hub);
    rsx! { {children} }
}

/// Button that shows a toast when pressed.
#[component]
pub fn ToastTrigger(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] variant: ToastType,
    #[props(into)] title: String,
    #[props(into, optional)] description: Option<String>,
    #[props(default)] position: ToastPosition,
    children: Element,
) -> Element {
    let hub = use_context::<ToastHub>();
    let merged = tw_merge!(
        TRIGGER_BASE,
        variant.trigger_class(),
        class.as_deref().unwrap_or("")
    );
    let description_attr = description.clone().unwrap_or_default();
    let shown_title = title.clone();
    rsx! {
        button {
            r#type: "button",
            "data-name": "ToastTrigger",
            "data-variant": "{variant.as_str()}",
            "data-toast-title": "{title}",
            "data-toast-description": "{description_attr}",
            "data-toast-position": "{position.as_str()}",
            class: "{merged}",
            onclick: move |_| hub.push(variant, shown_title.clone(), description.clone(), position),
            {children}
        }
    }
}

/// Positioned container for one toaster. Use `ToastToaster` unless the
/// placement needs a custom layout.
#[component]
pub fn ToastContainer(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] position: ToastPosition,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "toast__container fixed z-50",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { class: "{merged}", "data-position": "{position.as_str()}", {children} } }
}

/// The `ol` that stacks toast items. Hovering it pauses timers and marks it
/// `data-expanded`.
#[component]
pub fn ToastList(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] position: ToastPosition,
    #[props(default)] direction: ToastDirection,
    #[props(optional)] expanded: Option<bool>,
    children: Element,
) -> Element {
    let hub = use_context::<ToastHub>();
    let mut hovered = hub.hovered;
    let is_expanded = expanded.unwrap_or(*hub.hovered.read());
    let merged = tw_merge!(LIST_CLASS, class.as_deref().unwrap_or(""));
    rsx! {
        ol {
            "data-name": "ToastList",
            "data-toast-toaster": "true",
            "data-toast-theme": "light",
            "data-position": "{position.as_str()}",
            "data-expanded": "{is_expanded}",
            "data-direction": "{direction.as_str()}",
            class: "{merged}",
            style: TOAST_CSS_VARS,
            onmouseenter: move |_| hovered.set(true),
            onmouseleave: move |_| hovered.set(false),
            {children}
        }
    }
}

/// Renders the queued toasts for `position`, newest at the front.
#[component]
pub fn ToastToaster(#[props(default)] position: ToastPosition) -> Element {
    let hub = use_context::<ToastHub>();
    let snapshot: Vec<ToastEntry> = hub
        .entries
        .read()
        .iter()
        .filter(|entry| entry.position == position)
        .cloned()
        .collect();
    let expanded = *hub.hovered.read();
    let total = snapshot.len();
    let front_index = snapshot.iter().rposition(|entry| !entry.removed);
    rsx! {
        ToastContainer { class: position.container_class(), position,
            ToastList { position, expanded,
                for (index, entry) in snapshot.into_iter().enumerate() {
                    ToastItem {
                        key: "{entry.id}",
                        front: front_index == Some(index),
                        entry,
                        index,
                        total,
                        expanded,
                    }
                }
            }
        }
    }
}

/// One toast row. Layout and `data-*` state come from the entry; the JS
/// bridge's stacking transforms are not reproduced (see the report).
#[component]
fn ToastItem(
    entry: ToastEntry,
    index: usize,
    total: usize,
    front: bool,
    expanded: bool,
) -> Element {
    let hub = use_context::<ToastHub>();
    let from_end = total - 1 - index;
    let visible = from_end < VISIBLE_TOASTS_AMOUNT;
    let hidden = from_end >= MAX_TOASTS;
    let item_class = format!("{ITEM_BASE} {}", entry.variant.item_class());
    let style = format!("--index: {index}; --toasts-before: {index}; --z-index: {index};");
    let loading = entry.variant == ToastType::Loading;
    let icon_class = if loading {
        format!("{ICON_CLASS} [&>svg]:animate-spin")
    } else {
        ICON_CLASS.to_string()
    };
    let timed = !loading;
    let id = entry.id;
    rsx! {
        li {
            "data-name": "ToastItem",
            "data-toast-toast": "true",
            "data-variant": "{entry.variant.as_str()}",
            "data-mounted": "{entry.mounted}",
            "data-entering": "{entry.entering}",
            "data-expanded": "{expanded}",
            "data-visible": "{visible}",
            "data-hidden": "{hidden}",
            "data-swiping": "false",
            "data-removed": "{entry.removed}",
            "data-swipe-out": "false",
            "data-front": "{front}",
            class: "{item_class}",
            style: "{style}",
            if let Some(icon) = entry.variant.icon() {
                div { "data-icon": "", class: "{icon_class}",
                    Icon { icon, class: "size-5" }
                }
            }
            div { class: "flex-1",
                div { class: "flex items-center gap-2",
                    h3 { class: "font-semibold text-base leading-[1.4] flex-1", "{entry.title}" }
                    if !loading {
                        button {
                            r#type: "button",
                            "data-close-button": "",
                            "aria-label": "Close toast",
                            class: CLOSE_CLASS,
                            onclick: move |_| hub.dismiss(id),
                            Icon { icon: IconType::X }
                        }
                    }
                }
                if let Some(description) = entry.description.clone() {
                    p { class: "text-sm leading-[1.5] opacity-90 mt-1", "{description}" }
                }
            }
            if timed {
                div { "data-duration-track": "", class: TRACK_CLASS,
                    div {
                        "data-duration-progress": "",
                        class: PROGRESS_CLASS,
                        style: "transform: scaleX({entry.progress}); transition: transform 50ms linear;",
                    }
                }
            }
        }
    }
}
