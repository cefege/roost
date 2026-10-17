//! `Spinner`: the loading glyph a running turn or a pending request shows. Decorative,
//! hidden from assistive technology; the row's label carries the state. `SpinnerCircle`
//! is rust-ui's ring variant for use inside a button.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::{Icon, IconType};

#[component]
pub fn Spinner(#[props(into, optional)] class: Option<String>) -> Element {
    let merged = tw_merge!("size-4 animate-spin", class.as_deref().unwrap_or(""));
    rsx! { Icon { icon: IconType::LoaderCircle, class: merged } }
}

#[component]
pub fn SpinnerCircle(#[props(into, optional)] class: Option<String>) -> Element {
    let merged = tw_merge!("size-4 animate-spin", class.as_deref().unwrap_or(""));
    rsx! { Icon { icon: IconType::Loader, class: merged } }
}
