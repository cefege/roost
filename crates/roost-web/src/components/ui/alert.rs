//! `Alert`, `AlertTitle` and `AlertDescription`: an inline notice that a state
//! needs attention. `Destructive` marks a failure the user must act on; `Warning`
//! marks a condition to watch that does not block. `role` is `alert` unless the
//! notice waits rather than interrupts, then `status`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlertVariant {
    #[default]
    Default,
    Destructive,
    Warning,
}

impl AlertVariant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "bg-background text-foreground",
            Self::Destructive => {
                "border-destructive/50 text-destructive dark:border-destructive [&>svg]:text-destructive"
            }
            Self::Warning => {
                "border-warning/50 text-warning dark:border-warning [&>svg]:text-warning"
            }
        }
    }
}

#[component]
pub fn Alert(
    #[props(default)] variant: AlertVariant,
    #[props(into, default = "alert".to_owned())] role: String,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, default)] class: Option<String>,
    #[props(into, optional)] aria_live: Option<String>,
    #[props(into, optional)] aria_atomic: Option<String>,
    children: Element,
) -> Element {
    let class = tw_merge!(
        "relative w-full rounded-lg border px-4 py-3 text-sm [&>svg+div]:translate-y-[-3px] [&>svg]:absolute [&>svg]:left-4 [&>svg]:top-4 [&>svg]:text-foreground [&>svg~*]:pl-7",
        variant.as_str(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { role: "{role}", class: "{class}", "data-testid": test_id.as_deref(), "aria-live": aria_live.as_deref(), "aria-atomic": aria_atomic.as_deref(), {children} }
    }
}

#[component]
pub fn AlertTitle(#[props(into, default)] class: Option<String>, children: Element) -> Element {
    let class = tw_merge!(
        "mb-1 font-medium tracking-tight leading-none",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        h4 { class: "{class}", {children} }
    }
}

#[component]
pub fn AlertDescription(
    #[props(into, default)] class: Option<String>,
    children: Element,
) -> Element {
    let class = tw_merge!(
        "text-sm [&_p]:leading-relaxed",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        p { class: "{class}", {children} }
    }
}
