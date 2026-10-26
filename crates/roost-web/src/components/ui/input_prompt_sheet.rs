//! `InputPromptSheet`: the composer frame. The pi-pocket composer is a rounded
//! rectangle, not a pill, so this wraps `InputPrompt` and swaps only its corner
//! radius to the `--radius-sheet` token. The stock `InputPrompt` and `InputGroup`
//! recipes stay untouched beside it; any other prompt keeps the rust-ui control radius.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::input_prompt::InputPrompt;

#[component]
pub fn InputPromptSheet(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!("rounded-sheet", class.as_deref().unwrap_or(""));
    rsx! { InputPrompt { class: merged, attributes, {children} } }
}
