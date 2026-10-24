//! The folder picker's path band: history and parent buttons, the breadcrumb
//! trail with its overflow menu, and the width measurement strip the collapse
//! reads. Filtering swaps the trail for the filter field. Called by
//! `browse::picker`; the trail collapse is `platform::worker_paths::palette`.

use crate::platform::worker_paths::WorkerPathCrumb;
use crate::platform::worker_paths::palette::CrumbView;
use dioxus::prelude::*;
use libero::components::Box;
use libero::components::{
    ActionIcon, Button, Flex, Menu, MenuItem, Paper, Pictogram, TextField, use_menu,
};
use libero::sx::sx;

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
    let band_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .padding_inline("sm")
            .padding_block("xs")
    });
    let filter_sx = use_hook(|| sx().min_width("0").flex("1"));
    let crumbs_sx = use_hook(|| {
        sx().position("relative")
            .display("flex")
            .min_width("0")
            .flex("1")
            .align_items("center")
            .gap("xs")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .white_space("nowrap")
    });
    let measure_sx = use_hook(|| {
        sx().position("absolute")
            .top("0")
            .left("0")
            .display("flex")
            .align_items("center")
            .gap("xs")
            .white_space("nowrap")
            .visibility("hidden")
            .pointer_events("none")
    });
    let chevron_sx = use_hook(|| {
        sx().width("0.75rem")
            .height("0.75rem")
            .flex_shrink("0")
            .color("muted")
    });
    let crumb_sx = use_hook(|| {
        sx().max_width("12rem")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
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
    rsx! {
        Paper { shadow: "none", radius: "0", sx: band_sx,
            if filter_open {
                    TextField {
                        sx: filter_sx,
                        id: crate::components::browse::dom::FILTER_ID,
                        value: Some(filter.clone()),
                        oninput: move |value: String| on_filter.call(value),
                        placeholder: "Filter this folder",
                        "aria-label": "Filter this folder",
                        "data-testid": "browse-filter",
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Named(NamedKey::Escape) {
                                event.prevent_default();
                                on_close_filter.call(());
                            }
                        },
                    }
                    ActionIcon {
                        size: "sm",
                        "data-testid": "browse-filter-close",
                        icon: pictogram_icons_lucide::x::outlined,
                        aria_label: "Close filter",
                        title: "Close filter",
                        onclick: move |_| on_close_filter.call(()),
                    }
                } else {
                    ActionIcon {
                        size: "sm",
                        "data-testid": "browse-back",
                        icon: pictogram_icons_lucide::arrow_left::outlined,
                        aria_label: "Back",
                        title: "Back",
                        disabled: !back_enabled,
                        onclick: move |_| on_back.call(()),
                    }
                    ActionIcon {
                        size: "sm",
                        "data-testid": "browse-forward",
                        icon: pictogram_icons_lucide::arrow_right::outlined,
                        aria_label: "Forward",
                        title: "Forward",
                        disabled: !forward_enabled,
                        onclick: move |_| on_forward.call(()),
                    }
                    ActionIcon {
                        size: "sm",
                        "data-testid": "browse-up",
                        icon: pictogram_icons_lucide::arrow_up::outlined,
                        aria_label: "Parent folder",
                        title: "Parent folder",
                        disabled: !up_enabled,
                        onclick: move |_| on_up.call(()),
                    }
                    ActionIcon {
                        size: "sm",
                        "data-testid": "browse-home",
                        icon: pictogram_icons_lucide::house::outlined,
                        aria_label: "Home folder",
                        title: "Home folder",
                        onclick: move |_| on_home.call(()),
                    }
                    Flex { sx: crumbs_sx, direction: "row",
                        id: CRUMBS_ID,
                        "data-testid": "browse-crumbs",
                        for (index, view) in crumb_views.iter().enumerate() {
                            if index > 0 {
                                Box { component: "span", sx: chevron_sx.clone(), Pictogram { icon: pictogram_icons_lucide::chevron_right::outlined, width: "0.75rem", height: "0.75rem" } }
                            }
                            match view {
                                CrumbView::Crumb(crumb) => {
                                    let target = crumb.path.clone();
                                    rsx! {
                                        Button {
                                            sx: crumb_sx.clone(),
                                            variant: if index == last_index { "tonal" } else { "standard" },
                                            size: "sm",
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
                                    CrumbOverflowMenu {
                                        crumbs: menu_crumbs.clone(),
                                        open: menu_open,
                                        on_navigate,
                                        on_toggle: on_toggle_menu,
                                        on_close: on_close_menu,
                                    }
                                },
                            }
                        }
                    }
                    Box {
                        sx: measure_sx,
                        id: CRUMBS_MEASURE_ID,
                        "aria-hidden": "true",
                        for (index, crumb) in crumbs.iter().enumerate() {
                            if index > 0 {
                                Box { component: "span", sx: chevron_sx.clone(), Pictogram { icon: pictogram_icons_lucide::chevron_right::outlined, width: "0.75rem", height: "0.75rem" } }
                            }
                            Button {
                                variant: "standard",
                                size: "sm",
                                "data-mirror-crumb": "true",
                                tabindex: "-1",
                                {crumb.label.clone()}
                            }
                        }
                        Box { component: "span", sx: chevron_sx.clone(), Pictogram { icon: pictogram_icons_lucide::chevron_right::outlined, width: "0.75rem", height: "0.75rem" } }
                        ActionIcon {
                            size: "sm",
                            icon: pictogram_icons_lucide::ellipsis::outlined,
                            aria_label: "Show hidden folders",
                            "data-mirror-overflow": "true",
                            tabindex: "-1",
                        }
                    }
                }
            }
    }
}
#[component]
fn CrumbOverflowMenu(
    crumbs: Vec<WorkerPathCrumb>,
    open: bool,
    on_navigate: EventHandler<String>,
    on_toggle: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let trigger_sx = use_hook(|| {
        sx().width("1.75rem")
            .height("1.75rem")
            .padding("0")
            .color("muted")
            .flex_shrink("0")
    });
    let sr_only_sx = use_hook(|| {
        sx().position("absolute")
            .width("1px")
            .height("1px")
            .padding("0")
            .margin("-1px")
            .overflow("hidden")
            .with("clip", "rect(0, 0, 0, 0)")
            .white_space("nowrap")
            .with("border", "0")
    });
    let icon_sx = use_hook(|| sx().width("1rem").height("1rem"));
    let menu = use_menu();
    use_effect(use_reactive((&open,), move |(open,)| {
        if open {
            menu.open();
        } else {
            menu.close();
        }
    }));
    let mut last_open = use_signal(|| menu.is_open());
    use_effect(move || {
        let is_open = menu.is_open();
        if is_open != *last_open.peek() {
            last_open.set(is_open);
            if is_open && !open {
                on_toggle.call(());
            } else if !is_open && open {
                on_close.call(());
            }
        }
    });
    let mut items = Vec::with_capacity(crumbs.len());
    for crumb in crumbs {
        let target = crumb.path.clone();
        items.push(
            MenuItem::new(crumb.label)
                .description(crumb.path)
                .onselect(move |_| on_navigate.call(target.clone()))
                .into(),
        );
    }
    rsx! {
        Menu { state: menu, items,
            Button {
                variant: "standard",
                size: "xs",
                sx: trigger_sx,
                attributes: menu.a11y_attributes(),
                "data-testid": "browse-crumb-overflow",
                Box { component: "span", sx: sr_only_sx, "Show hidden folders" }
                Box { component: "span", sx: icon_sx, Pictogram { icon: pictogram_icons_lucide::ellipsis::outlined, width: "1rem", height: "1rem" } }
            }
        }
    }
}
