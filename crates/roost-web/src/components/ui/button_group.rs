//! `ButtonGroup`: a row of joined buttons that share one border and one radius
//! at the outer edges. Toolbars and segmented controls compose it, with the
//! orientation picking which edges square off.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonGroupOrientation {
    #[default]
    Horizontal,
    Vertical,
}

impl ButtonGroupOrientation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => {
                "[&>*:not(:first-child)]:rounded-l-none [&>*:not(:first-child)]:border-l-0 [&>*:not(:last-child)]:rounded-r-none"
            }
            Self::Vertical => {
                "flex-col [&>*:not(:first-child)]:rounded-t-none [&>*:not(:first-child)]:border-t-0 [&>*:not(:last-child)]:rounded-b-none"
            }
        }
    }
}

#[component]
pub fn ButtonGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ButtonGroupOrientation::default())] orientation: ButtonGroupOrientation,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "flex w-fit items-stretch [&>*]:focus-visible:z-10 [&>*]:focus-visible:relative [&>[data-slot=select-trigger]:not([class*='w-'])]:w-fit [&>input]:flex-1 has-[select[aria-hidden=true]:last-child]:[&>[data-slot=select-trigger]:last-of-type]:rounded-r-md has-[>[data-slot=button-group]]:gap-2",
        orientation.as_str(),
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div { "data-name": "ButtonGroup", role: "group", class: "{merged_class}", {children} }
    }
}
