//! `List`: the container `ListRow`s sit in — a plain stack, a bordered
//! container, or a responsive grid of rows. Ported from
//! `apps/web/src/components/Settings/md/List.tsx`; every list surface composes
//! it. `tokens.css` owns the three layouts.

use dioxus::prelude::*;

use super::class_list::class_list;

/// How the rows flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListLayout {
    /// One row per line.
    #[default]
    Stack,
    /// `md-list--grid`: rows tile into columns.
    Grid,
}

/// The list's class attribute.
pub fn list_class(contained: bool, layout: ListLayout, class: Option<&str>) -> String {
    class_list([
        "md-list",
        if contained { "md-list--container" } else { "" },
        if layout == ListLayout::Grid {
            "md-list--grid"
        } else {
            ""
        },
        class.unwrap_or(""),
    ])
}

/// A list of rows.
#[component]
pub fn List(
    #[props(default)] contained: bool,
    #[props(default)] layout: ListLayout,
    class: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        div { class: list_class(contained, layout, class.as_deref()), {children} }
    }
}
