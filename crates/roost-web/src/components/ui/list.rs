//! `List` and `ListRow`: the container rows sit in and the Material-style row
//! anatomy (leading icon, headline, support line, trailing slot) for static
//! content, actions and destinations. Settings rails, folder lists and the
//! browse picker compose them.
//!
//! A row is an `<a>` when it has an `href`, so it keeps native link affordances
//! (open in new tab, copy address); a plain left click is handed to
//! `on_navigate` instead of loading a document. An action is a `<button>`, and
//! a static row is a `<div>`.

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::icon::Icon;
use crate::components::ui::IconType;

/// The attribute value `ListLayout::Grid` sets, which a caller that must find a
/// live grid in the document (the browse picker's column count) selects by.
pub const GRID_LAYOUT_SELECTOR: &str = "[data-layout=\"grid\"]";

/// How the rows flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListLayout {
    /// One row per line.
    #[default]
    Stack,
    /// Rows tile into as many columns as the width allows.
    Grid,
}

/// Whether a click on a destination row is in-app navigation: the primary button
/// with no modifier. A modifier or another button asks the browser for a new tab,
/// a window or a download, and that request is not the router's to swallow.
pub fn is_in_app_navigation_click(primary_button: bool, modifiers: Modifiers) -> bool {
    primary_button && modifiers.is_empty()
}

/// A list of rows.
#[component]
pub fn List(
    #[props(default)] contained: bool,
    #[props(default)] layout: ListLayout,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let layout_class = match layout {
        ListLayout::Stack => "flex flex-col",
        ListLayout::Grid => "grid gap-3 grid-cols-[repeat(auto-fill,minmax(12rem,1fr))]",
    };
    let container_class = if contained {
        "rounded-md border border-border overflow-hidden"
    } else {
        ""
    };
    let layout_attribute = match layout {
        ListLayout::Stack => "stack",
        ListLayout::Grid => "grid",
    };
    let merged = tw_merge!(layout_class, container_class, class.as_deref().unwrap_or(""));
    rsx! {
        div { "data-name": "List", "data-layout": "{layout_attribute}", class: "{merged}", {children} }
    }
}

/// A list row.
#[component]
pub fn ListRow(
    /// A leading glyph, drawn as an `Icon`.
    leading_icon: Option<IconType>,
    /// Leading content other than a plain glyph.
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
    /// Option-row wiring for a listbox: the row's id, role and `aria-selected`.
    id: Option<String>,
    role: Option<String>,
    aria_selected: Option<String>,
    tabindex: Option<i32>,
    class: Option<String>,
) -> Element {
    let density_class = if dense { "min-h-10 py-1" } else { "min-h-14 py-2" };
    let row_class = tw_merge!(
        "flex w-full items-center gap-4 px-4 text-left text-foreground rounded-md transition-colors hover:bg-accent/50 data-[selected=true]:bg-accent",
        density_class,
        class.as_deref().unwrap_or("")
    );
    let data_selected = selected.then_some("true");
    let inner = rsx! {
        if leading_icon.is_some() || leading.is_some() {
            div { class: "flex shrink-0 items-center justify-center gap-2 text-muted-foreground",
                if let Some(glyph) = leading_icon {
                    Icon { icon: glyph }
                }
                if let Some(leading) = leading {
                    {leading}
                }
            }
        }
        div { class: "flex min-w-0 flex-1 flex-col",
            div { class: "truncate overscroll-y-auto text-sm font-medium text-foreground", {headline} }
            if let Some(support) = support {
                div { class: "truncate overscroll-y-auto text-sm font-normal text-muted-foreground", {support} }
            }
        }
        if let Some(trailing) = trailing {
            div { class: "flex shrink-0 items-center gap-2", {trailing} }
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
                id,
                role,
                "aria-selected": aria_selected,
                tabindex,
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
