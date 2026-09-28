//! `ListRow`: the Material list-row anatomy — leading icon, headline, support
//! line, trailing slot — for static content, actions and destinations. Ported
//! from `apps/web/src/components/Settings/md/ListRow.tsx`; the settings rail,
//! folder lists and control specimens compose it.
//!
//! Element semantics follow `href` first, then `onclick`: a destination is an
//! `<a>` so it keeps native link affordances (open in new tab, copy address), an
//! action is a `<button>`, and a static row is a `<div>`. A destination's plain
//! left click is handed to `on_navigate` instead of loading a document, as v2's
//! router `<A>` did; a modified click stays the browser's.

use dioxus::prelude::*;
use dioxus::html::input_data::MouseButton;

use super::class_list::class_list;
use super::icon::Icon;

/// The row's class attribute.
pub fn list_row_class(dense: bool, class: Option<&str>) -> String {
    class_list([
        "md-list-row",
        if dense { "md-list-row--dense" } else { "" },
        class.unwrap_or(""),
    ])
}

/// Whether a click on a destination row is the in-app navigation v2's router
/// intercepted: the primary button with no modifier. Any modifier or another
/// button is the reader asking the browser for a new tab, a window or a
/// download, and that request is not the router's to swallow.
pub fn is_in_app_navigation_click(primary_button: bool, modifiers: Modifiers) -> bool {
    primary_button && modifiers.is_empty()
}

/// A list row.
#[component]
pub fn ListRow(
    /// A leading glyph, drawn as an `Icon`. v2 accepted a string here.
    leading_icon: Option<String>,
    /// Leading content other than a plain glyph. v2 accepted an element here.
    leading: Option<Element>,
    headline: Element,
    support: Option<Element>,
    trailing: Option<Element>,
    onclick: Option<EventHandler<()>>,
    href: Option<String>,
    /// Receives `href` on an in-app click; without it the anchor navigates natively.
    on_navigate: Option<EventHandler<String>>,
    #[props(default)] selected: bool,
    #[props(default)] dense: bool,
    aria_current: Option<String>,
    test_id: Option<String>,
    class: Option<String>,
) -> Element {
    let row_class = list_row_class(dense, class.as_deref());
    let data_selected = selected.then_some("true");
    let inner = rsx! {
        if leading_icon.is_some() || leading.is_some() {
            div { class: "md-list-row__leading",
                if let Some(glyph) = leading_icon {
                    Icon { name: glyph }
                }
                if let Some(leading) = leading {
                    {leading}
                }
            }
        }
        div { class: "md-list-row__body",
            div { class: "md-list-row__headline", {headline} }
            if let Some(support) = support {
                div { class: "md-list-row__support", {support} }
            }
        }
        if let Some(trailing) = trailing {
            div { class: "md-list-row__trailing", {trailing} }
        }
    };
    if let Some(href) = href {
        return rsx! {
            a {
                href: href.clone(),
                class: row_class,
                "data-selected": data_selected,
                "data-testid": test_id,
                "aria-current": aria_current,
                onclick: move |event: MouseEvent| {
                    let Some(navigate) = on_navigate else {
                        return;
                    };
                    let primary = event.trigger_button() == Some(MouseButton::Primary);
                    if is_in_app_navigation_click(primary, event.modifiers()) {
                        event.prevent_default();
                        navigate.call(href.clone());
                    }
                },
                {inner}
            }
        };
    }
    match onclick {
        Some(handler) => rsx! {
            button {
                r#type: "button",
                class: row_class,
                "data-selected": data_selected,
                "data-testid": test_id,
                onclick: move |_| handler.call(()),
                {inner}
            }
        },
        None => rsx! {
            div {
                class: row_class,
                "data-selected": data_selected,
                "data-testid": test_id,
                {inner}
            }
        },
    }
}
