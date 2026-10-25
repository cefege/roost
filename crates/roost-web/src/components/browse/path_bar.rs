//! The folder picker's path band: history and parent buttons, the breadcrumb
//! trail with its overflow menu, and the width measurement strip the collapse
//! reads. Filtering swaps the trail for the filter field. Called by
//! `browse::picker`; the trail collapse is `platform::worker_paths::palette`.

use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, TextField,
};
use crate::platform::worker_paths::WorkerPathCrumb;
use crate::platform::worker_paths::palette::CrumbView;
use dioxus::prelude::*;

/// The collapsed trail's element id.
pub const CRUMBS_ID: &str = "browse-crumbs";
/// The hidden full trail the collapse measures.
pub const CRUMBS_MEASURE_ID: &str = "browse-crumbs-measure";

/// The path band.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn BrowsePathBar(
    /// The collapsed trail the strip paints.
    crumb_views: Vec<CrumbView>,
    /// The full, uncollapsed trail the hidden strip measures.
    crumbs: Vec<WorkerPathCrumb>,
    /// Whether the overflow menu is showing.
    menu_open: bool,
    /// Whether Back is available.
    back_enabled: bool,
    /// Whether Forward is available.
    forward_enabled: bool,
    /// Whether Up has somewhere to go.
    up_enabled: bool,
    /// Whether the filter box is showing instead of the trail.
    filter_open: bool,
    /// The in-list filter text.
    filter: String,
    on_navigate: EventHandler<String>,
    on_back: EventHandler<()>,
    on_forward: EventHandler<()>,
    on_up: EventHandler<()>,
    on_home: EventHandler<()>,
    on_filter: EventHandler<String>,
    on_close_filter: EventHandler<()>,
    on_toggle_menu: EventHandler<()>,
    on_close_menu: EventHandler<()>,
) -> Element {
    let last_index = crumb_views.len().saturating_sub(1);
    // The overflow menu lists the folded middle, which only the collapsed view
    // knows about; flattening it here keeps the `let` out of the rsx body.
    let menu_crumbs: Vec<WorkerPathCrumb> = crumb_views
        .iter()
        .filter_map(|view| match view {
            CrumbView::Ellipsis(hidden) => Some(hidden.clone()),
            CrumbView::Crumb(_) => None,
        })
        .flatten()
        .collect();
    let mut menu_items: Vec<Element> = Vec::with_capacity(menu_crumbs.len());
    for crumb in &menu_crumbs {
        let target = crumb.path.clone();
        menu_items.push(rsx! {
            DropdownMenuItem {
                test_id: "browse-crumb-menu-item",
                class: "flex w-full items-center gap-2 text-left",
                title: crumb.path.clone(),
                onclick: move |_| on_navigate.call(target.clone()),
                span { {crumb.label.clone()} }
            }
        });
    }
    // The ui root reports the open state it wants; the page's toggle and close
    // callbacks stay the only owners of that state.
    let on_menu_open_change = EventHandler::new(move |open: bool| {
        if open {
            on_toggle_menu.call(());
        } else {
            on_close_menu.call(());
        }
    });
    rsx! {
        DropdownMenu {
            open: menu_open,
            on_open_change: on_menu_open_change,
            test_id: "browse-crumb-menu",
            div { class: "flex min-w-0 items-center gap-1 px-2 py-1",
                if filter_open {
                    TextField {
                        class: Some("min-w-0 flex-1".to_owned()),
                        control_class: Some("h-8".to_owned()),
                        id: Some(crate::components::browse::dom::FILTER_ID.to_owned()),
                        value: filter.clone(),
                        on_input: move |value: String| on_filter.call(value),
                        placeholder: Some("Filter this folder".to_owned()),
                        aria_label: Some("Filter this folder".to_owned()),
                        test_id: Some("browse-filter".to_owned()),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Named(NamedKey::Escape) {
                                event.prevent_default();
                                on_close_filter.call(());
                            }
                        },
                    }
                    IconButton {
                        size: ButtonSize::IconSm,
                        "data-testid": "browse-filter-close",
                        icon: IconType::X,
                        label: "Close filter",
                        title: "Close filter",
                        onclick: move |_| on_close_filter.call(()),
                    }
                } else {
                    IconButton {
                        size: ButtonSize::IconSm,
                        "data-testid": "browse-back",
                        icon: IconType::ArrowLeft,
                        label: "Back",
                        title: "Back",
                        disabled: !back_enabled,
                        onclick: move |_| on_back.call(()),
                    }
                    IconButton {
                        size: ButtonSize::IconSm,
                        "data-testid": "browse-forward",
                        icon: IconType::ArrowRight,
                        label: "Forward",
                        title: "Forward",
                        disabled: !forward_enabled,
                        onclick: move |_| on_forward.call(()),
                    }
                    IconButton {
                        size: ButtonSize::IconSm,
                        "data-testid": "browse-up",
                        icon: IconType::ArrowUp,
                        label: "Parent folder",
                        title: "Parent folder",
                        disabled: !up_enabled,
                        onclick: move |_| on_up.call(()),
                    }
                    IconButton {
                        size: ButtonSize::IconSm,
                        "data-testid": "browse-home",
                        icon: IconType::House,
                        label: "Home folder",
                        title: "Home folder",
                        onclick: move |_| on_home.call(()),
                    }
                    div {
                        class: "relative flex min-w-0 flex-1 items-center gap-1 overflow-hidden whitespace-nowrap",
                        id: CRUMBS_ID,
                        "data-testid": "browse-crumbs",
                        for (index, view) in crumb_views.iter().enumerate() {
                            if index > 0 {
                                Icon { icon: IconType::ChevronRight, class: "size-3 shrink-0 text-muted-foreground" }
                            }
                            match view {
                                CrumbView::Crumb(crumb) => {
                                    let target = crumb.path.clone();
                                    rsx! {
                                        Button {
                                            class: Some("max-w-48 truncate".to_owned()),
                                            variant: if index == last_index { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                                            size: ButtonSize::Sm,
                                            "data-testid": "browse-crumb",
                                            "data-current": (index == last_index).then_some("true"),
                                            "aria-current": (index == last_index).then(|| "page".to_owned()),
                                            title: crumb.path.clone(),
                                            onclick: move |_| on_navigate.call(target.clone()),
                                            {crumb.label.clone()}
                                        }
                                    }
                                }
                                CrumbView::Ellipsis(_) => rsx! {
                                    DropdownMenuTrigger {
                                        class: "shrink-0",
                                        ghost: true,
                                        test_id: "browse-crumb-overflow",
                                        span { class: "sr-only", "Show hidden folders" }
                                        Icon { icon: IconType::Ellipsis }
                                    }
                                },
                            }
                        }
                    }
                    div {
                        class: "pointer-events-none invisible absolute top-0 left-0 flex items-center gap-1 whitespace-nowrap",
                        id: CRUMBS_MEASURE_ID,
                        "aria-hidden": "true",
                        for (index, crumb) in crumbs.iter().enumerate() {
                            if index > 0 {
                                Icon { icon: IconType::ChevronRight, class: "size-3 shrink-0" }
                            }
                            Button {
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Sm,
                                "data-mirror-crumb": "true",
                                tabindex: "-1",
                                {crumb.label.clone()}
                            }
                        }
                        Icon { icon: IconType::ChevronRight, class: "size-3 shrink-0" }
                        IconButton {
                            size: ButtonSize::IconSm,
                            icon: IconType::Ellipsis,
                            label: "Show hidden folders",
                            "data-mirror-overflow": "true",
                            tabindex: "-1",
                        }
                    }
                }
            }
            DropdownMenuContent {
                class: "w-auto min-w-48 max-h-[60vh] overflow-y-auto",
                {menu_items.into_iter()}
            }
        }
    }
}
