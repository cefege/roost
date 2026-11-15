//! `ToastPresentation`, the one toast card: rust-ui's item markup and `data-*`
//! hooks on the popover surface, severity in the icon colour alone, an actions
//! slot in the title row, a 1px duration/progress track. Rendered by
//! `components/notifications/` cards and `/design`; depends on `ui::Icon`.

use dioxus::prelude::*;
use icons::common::IconType;
use tw_merge::tw_merge;

use super::icon::Icon;

const ITEM_BASE: &str = "relative w-full min-w-0 overflow-hidden px-3 py-2.5 shadow-md border border-border rounded-lg bg-popover text-popover-foreground flex items-start gap-2.5";
const ICON_CLASS: &str = "flex items-center justify-center size-5 shrink-0";
const LOADING_SPIN: &str = "[&>svg]:animate-spin motion-reduce:[&>svg]:animate-none";
const CLOSE_CLASS: &str = "size-5 flex items-center justify-center rounded-sm border-none bg-transparent text-muted-foreground cursor-pointer transition-colors duration-150 shrink-0 p-0 ml-auto hover:text-foreground";
const TRACK_CLASS: &str = "absolute bottom-0 inset-x-0 h-px bg-border overflow-hidden";
const PROGRESS_CLASS: &str = "h-full w-full bg-muted-foreground opacity-50 origin-left";

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

    /// The icon's colour: the only place severity shows.
    const fn icon_tone(self) -> &'static str {
        match self {
            Self::Default | Self::Loading => "text-muted-foreground",
            Self::Success => "text-success",
            Self::Error => "text-destructive",
            Self::Warning => "text-warning",
            Self::Info => "text-info",
        }
    }

    const fn icon(self) -> Option<IconType> {
        match self {
            Self::Default => None,
            Self::Success => Some(IconType::Check),
            Self::Error => Some(IconType::X),
            Self::Warning => Some(IconType::TriangleAlert),
            Self::Info => Some(IconType::Info),
            Self::Loading => Some(IconType::LoaderCircle),
        }
    }
}

/// One toast card: icon, title row (title, `actions`, close), description,
/// `children`, and an optional progress track along the bottom edge.
#[component]
pub fn ToastPresentation(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] variant: ToastType,
    #[props(into)] title: String,
    #[props(into, optional)] description: Option<String>,
    #[props(into, optional)] role: Option<String>,
    #[props(into, optional)] aria_live: Option<String>,
    #[props(into, optional)] aria_atomic: Option<String>,
    #[props(into, optional)] close_label: Option<String>,
    #[props(optional)] on_close: Option<EventHandler<MouseEvent>>,
    #[props(optional)] progress: Option<f64>,
    #[props(into, optional)] progress_class: Option<String>,
    #[props(into, optional)] progress_style: Option<String>,
    #[props(optional)] actions: Option<Element>,
    children: Element,
) -> Element {
    let merged = tw_merge!(ITEM_BASE, class.as_deref().unwrap_or(""));
    let icon_class = tw_merge!(
        ICON_CLASS,
        variant.icon_tone(),
        if variant == ToastType::Loading {
            LOADING_SPIN
        } else {
            ""
        }
    );
    let close_aria = close_label.as_deref().unwrap_or("Close toast");
    let icon = variant.icon();
    rsx! {
        div {
            class: "{merged}",
            "data-variant": variant.as_str(),
            role: role,
            "aria-live": aria_live,
            "aria-atomic": aria_atomic,
            if let Some(icon) = icon {
                div { "data-icon": "", class: "{icon_class}",
                    Icon { icon, class: "size-4" }
                }
            }
            div { class: "flex-1 min-w-0",
                div { class: "flex items-center gap-2",
                    h3 { class: "font-medium text-sm leading-5 min-w-0 flex-1", title: "{title}", "{title}" }
                    if let Some(actions) = actions {
                        div { class: "flex shrink-0 items-center gap-1", {actions} }
                    }
                    if let Some(on_close) = on_close {
                        button {
                            r#type: "button",
                            "data-close-button": "",
                            "aria-label": "{close_aria}",
                            class: CLOSE_CLASS,
                            onclick: move |event: MouseEvent| {
                                event.stop_propagation();
                                on_close.call(event);
                            },
                            Icon { icon: IconType::X, class: "size-3" }
                        }
                    }
                }
                if let Some(description) = description {
                    p { class: "text-sm leading-5 text-muted-foreground mt-0.5", "{description}" }
                }
                {children}
            }
            if let Some(progress) = progress {
                div { "data-duration-track": "", class: TRACK_CLASS,
                    div {
                        "data-duration-progress": "",
                        class: "{PROGRESS_CLASS} {progress_class.as_deref().unwrap_or(\"\")}",
                        style: "{progress_style.as_deref().unwrap_or(\"\")} transform: scaleX({progress});",
                    }
                }
            }
        }
    }
}
