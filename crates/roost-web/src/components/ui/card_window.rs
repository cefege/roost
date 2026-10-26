//! `CardWindow`: the stock `Card` with the window radius and card elevation. The
//! operator chose the pi-pocket look for peek tiles, so this wraps `Card` and
//! swaps only its corner radius and shadow to the `--radius-window` and
//! `--shadow-card` tokens. The stock `Card` recipe stays untouched beside it;
//! every other card keeps the rust-ui control radius.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::card::{Card, CardSize};

#[component]
pub fn CardWindow(
    #[props(default)] size: CardSize,
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!("rounded-window shadow-card", class.as_deref().unwrap_or(""));
    rsx! { Card { size, class: Some(merged), attributes, {children} } }
}
