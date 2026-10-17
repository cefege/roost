//! The open select's floating listbox: the content surface, the `listbox` and
//! its `option`s, keyboard movement, type-ahead and dismissal. Rendered by
//! `select.rs` only while open; the rules it applies are `select_navigation`'s
//! and `select_placement`'s, and its browser readings come from `ui::dom`.
//!
//! Focus sits on the `listbox` and the highlighted option is named by
//! `aria-activedescendant`, so every option stays out of the tab order while
//! the arrows move through them; focus leaving the listbox is a dismissal.

use std::rc::Rc;

use dioxus::prelude::*;
use icons::IconType;

use super::Icon;
use super::dom::{key_event_time_ms, viewport_height};
use super::select::SelectOption;
use super::select_navigation::{
    ListboxKeyAction, Typeahead, listbox_key_action, typeahead_character, typeahead_match,
};
use super::select_placement::{AnchorRect, listbox_placement, listbox_style};

/// The surface the listbox sits on. Its position is the inline style from
/// `listbox_style`, so only the paint and the scroll bounds are classes here.
const LISTBOX_CONTENT_CLASS: &str = "z-50 overflow-auto p-1 rounded-md border bg-card shadow-md h-fit max-h-[300px] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden";

const LISTBOX_CLASS: &str = "m-0 list-none p-0 outline-hidden";

/// Rust/UI's select option recipe, with the highlight as a presence variant
/// because keyboard movement highlights without moving focus.
const OPTION_CLASS: &str = "group inline-flex gap-2 items-center w-full rounded-sm px-2 py-1.5 text-sm cursor-pointer no-underline transition-colors duration-200 text-popover-foreground hover:bg-accent hover:text-accent-foreground data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground [&_svg:not([class*='size-'])]:size-4";

const OPTION_CHECK_CLASS: &str = "ml-auto opacity-0 size-4 text-muted-foreground group-aria-selected:opacity-100";

/// The DOM id of one option, derived from its listbox.
pub fn select_option_id(listbox_id: &str, index: usize) -> String {
    format!("{listbox_id}-option-{index}")
}

/// Scroll an option into view without jumping the list.
fn reveal(item: &Rc<MountedData>) {
    let scroll = item.scroll_to_with_options(ScrollToOptions {
        behavior: ScrollBehavior::Instant,
        vertical: ScrollLogicalPosition::Nearest,
        horizontal: ScrollLogicalPosition::Nearest,
    });
    spawn(async move {
        let _ = scroll.await;
    });
}

/// The listbox. `on_dismiss(true)` closes and hands focus back to the trigger;
/// `on_dismiss(false)` closes because focus already went elsewhere.
#[component]
pub fn SelectListbox(
    options: Vec<SelectOption>,
    selected: Option<usize>,
    highlighted: Signal<Option<usize>>,
    anchor: AnchorRect,
    listbox_id: String,
    label_id: Option<String>,
    on_choose: EventHandler<usize>,
    on_dismiss: EventHandler<bool>,
) -> Element {
    let mut highlighted = highlighted;
    let mut content_height = use_signal(|| None::<f64>);
    let mut typeahead = use_signal(Typeahead::default);
    let mut items = use_signal(Vec::<Option<Rc<MountedData>>>::new);
    let placement = listbox_placement(anchor, content_height(), viewport_height());
    let active_descendant = highlighted().map(|index| select_option_id(&listbox_id, index));
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    let count = options.len();

    let mut move_highlight = move |index: usize| {
        highlighted.set(Some(index));
        if let Some(Some(item)) = items.read().get(index) {
            reveal(item);
        }
    };

    rsx! {
        div {
            class: LISTBOX_CONTENT_CLASS,
            "data-expanded": "",
            style: listbox_style(&placement),
            onmounted: move |event: MountedEvent| async move {
                if let Ok(rect) = event.data().get_client_rect().await {
                    content_height.set(Some(rect.size.height));
                }
            },
            ul {
                id: listbox_id.clone(),
                class: LISTBOX_CLASS,
                role: "listbox",
                tabindex: "-1",
                "aria-labelledby": label_id,
                "aria-activedescendant": active_descendant,
                onmounted: move |event: MountedEvent| async move {
                    let _ = event.data().set_focus(true).await;
                },
                onfocusout: move |_| on_dismiss.call(false),
                onkeydown: move |event: KeyboardEvent| {
                    let key = event.key().to_string();
                    let now = key_event_time_ms(&event);
                    let chord = event.modifiers().ctrl() || event.modifiers().meta();
                    let typing = typeahead_character(&key, chord)
                        .filter(|character| *character != " " || typeahead.peek().is_active(now));
                    if let Some(character) = typing {
                        event.prevent_default();
                        let query = typeahead.write().push(character, now).to_string();
                        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        if let Some(index) = typeahead_match(&label_refs, &query, highlighted()) {
                            move_highlight(index);
                        }
                        return;
                    }
                    match listbox_key_action(&key, count, highlighted()) {
                        ListboxKeyAction::Highlight(index) => {
                            event.prevent_default();
                            move_highlight(index);
                        }
                        ListboxKeyAction::Choose(index) => {
                            event.prevent_default();
                            on_choose.call(index);
                        }
                        ListboxKeyAction::Close => {
                            event.prevent_default();
                            event.stop_propagation();
                            on_dismiss.call(true);
                        }
                        ListboxKeyAction::Ignore => {}
                    }
                },
                for (index, option) in options.into_iter().enumerate() {
                    li {
                        key: "{option.value}",
                        id: select_option_id(&listbox_id, index),
                        class: OPTION_CLASS,
                        role: "option",
                        "aria-selected": (selected == Some(index)).to_string(),
                        "aria-labelledby": format!("{}-label", select_option_id(&listbox_id, index)),
                        "data-key": option.value.clone(),
                        "data-selected": (selected == Some(index)).then_some(""),
                        "data-highlighted": (highlighted() == Some(index)).then_some(""),
                        onmounted: move |event: MountedEvent| {
                            let mounted = event.data();
                            if highlighted() == Some(index) {
                                reveal(&mounted);
                            }
                            let mut slots = items.write();
                            if slots.len() <= index {
                                slots.resize(index + 1, None);
                            }
                            slots[index] = Some(mounted);
                        },
                        onpointermove: move |_| {
                            if highlighted() != Some(index) {
                                highlighted.set(Some(index));
                            }
                        },
                        onclick: move |_| on_choose.call(index),
                        div { id: format!("{}-label", select_option_id(&listbox_id, index)), {option.label} }
                        Icon { icon: IconType::Check, class: OPTION_CHECK_CLASS }
                    }
                }
            }
        }
    }
}
