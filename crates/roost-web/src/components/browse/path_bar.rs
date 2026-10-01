//! The folder picker's path band: history and parent navigation beside the
//! collapsed breadcrumb trail and its overflow menu, plus the measurement strip
//! the width-aware collapse reads. The same band carries the filter field
//! instead of the trail while filtering, so filtering costs the surface no
//! height. The page owns the collapse count, both element ids, and every value
//! read here.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/BrowsePathBar.tsx`; the trail itself is
//! `platform::worker_paths::palette`'s `collapse_crumbs_to`.

use crate::platform::worker_paths::WorkerPathCrumb;
use dioxus::prelude::*;

use crate::components::context_menu::{
    CtxMenuItem, MenuFocusEdge, ctx_menu_surface_style, focus_menu_edge, use_floating_menu_dismiss,
};
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconSize, Surface,
    SurfaceRadius, TextField,
};
use crate::platform::worker_paths::palette::CrumbView;

/// The collapsed trail's element id.
pub const CRUMBS_ID: &str = "browse-crumbs";
/// The hidden full trail the collapse measures.
pub const CRUMBS_MEASURE_ID: &str = "browse-crumbs-measure";
/// The overflow trigger's element id.
pub const CRUMB_OVERFLOW_ID: &str = "browse-crumb-overflow";
/// The overflow menu's element id.
pub const CRUMB_MENU_ID: &str = "browse-crumb-menu";

/// Where the overflow menu sits: under its trigger, left-aligned to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrumbMenuPos {
    /// The trigger's left edge, in client px.
    pub left: f64,
    /// The trigger's bottom edge, in client px.
    pub top: f64,
}

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
    /// The overflow menu's anchor, measured when it opened.
    menu_pos: Option<CrumbMenuPos>,
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
    use_floating_menu_dismiss(
        on_close_menu,
        None,
        vec![CRUMB_OVERFLOW_ID.to_owned(), CRUMB_MENU_ID.to_owned()],
    );
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
            CtxMenuItem {
                testid: "browse-crumb-menu-item".to_owned(),
                class: "df-browse-crumb-menu-item",
                title: crumb.path.clone(),
                onclick: move |_| on_navigate.call(target.clone()),
                span { {crumb.label.clone()} }
            }
        });
    }
    rsx! {
        Surface { class: "df-browse-path", level: 1, radius: SurfaceRadius::None,
            if filter_open {
                TextField {
                    class: "df-browse-filter",
                    id: Some(crate::components::browse::dom::FILTER_ID.to_owned()),
                    value: filter.clone(),
                    on_input: move |value: String| on_filter.call(value),
                    placeholder: "Filter this folder".to_owned(),
                    aria_label: Some("Filter this folder".to_owned()),
                    test_id: Some("browse-filter".to_owned()),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Escape {
                            event.prevent_default();
                            on_close_filter.call(());
                        }
                    },
                }
                IconButton {
                    size: IconButtonSize::IconSm,
                    "data-testid": "browse-filter-close",
                    icon: "close",
                    label: "Close filter".to_owned(),
                    title: "Close filter".to_owned(),
                    onclick: move |_| on_close_filter.call(()),
                }
            } else {
                IconButton {
                    size: IconButtonSize::IconSm,
                    "data-testid": "browse-back",
                    icon: "arrow_back",
                    label: "Back".to_owned(),
                    title: "Back".to_owned(),
                    disabled: !back_enabled,
                    onclick: move |_| on_back.call(()),
                }
                IconButton {
                    size: IconButtonSize::IconSm,
                    "data-testid": "browse-forward",
                    icon: "arrow_forward",
                    label: "Forward".to_owned(),
                    title: "Forward".to_owned(),
                    disabled: !forward_enabled,
                    onclick: move |_| on_forward.call(()),
                }
                IconButton {
                    size: IconButtonSize::IconSm,
                    "data-testid": "browse-up",
                    icon: "arrow_upward",
                    label: "Parent folder".to_owned(),
                    title: "Parent folder".to_owned(),
                    disabled: !up_enabled,
                    onclick: move |_| on_up.call(()),
                }
                IconButton {
                    size: IconButtonSize::IconSm,
                    "data-testid": "browse-home",
                    icon: "home",
                    label: "Home folder".to_owned(),
                    title: "Home folder".to_owned(),
                    onclick: move |_| on_home.call(()),
                }
                div { class: "df-browse-crumbs", id: CRUMBS_ID, "data-testid": "browse-crumbs",
                    for (index, view) in crumb_views.iter().enumerate() {
                        if index > 0 {
                            Icon { name: "chevron_right", size: IconSize::Sm, class: "df-browse-crumb-sep" }
                        }
                        match view {
                            CrumbView::Crumb(crumb) => {
                                let target = crumb.path.clone();
                                rsx! {
                                    Button {
                                        class: "df-browse-crumb",
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
                                IconButton {
                                    id: CRUMB_OVERFLOW_ID,
                                    class: "df-browse-crumb-overflow",
                                    "data-testid": "browse-crumb-overflow",
                                    size: IconButtonSize::IconSm,
                                    icon: "more_horiz",
                                    label: "Show hidden folders".to_owned(),
                                    title: "Show hidden folders".to_owned(),
                                    menu_popup: Some("menu".to_owned()),
                                    controls_id: Some(CRUMB_MENU_ID.to_owned()),
                                    expanded: Some(menu_open),
                                    onclick: move |_| on_toggle_menu.call(()),
                                    onkeydown: move |event: KeyboardEvent| {
                                        match event.key() {
                                            Key::ArrowDown | Key::ArrowUp => {
                                                event.prevent_default();
                                                event.stop_propagation();
                                                focus_menu_edge(
                                                    CRUMB_MENU_ID,
                                                    if event.key() == Key::ArrowDown { MenuFocusEdge::First } else { MenuFocusEdge::Last },
                                                );
                                            }
                                            Key::Escape if menu_open => {
                                                event.prevent_default();
                                                event.stop_propagation();
                                                on_close_menu.call(());
                                            }
                                            _ => {}
                                        }
                                    },
                                }
                            },
                        }
                    }
                }
                div { class: "df-browse-crumbs-measure", id: CRUMBS_MEASURE_ID, "aria-hidden": "true",
                    for (index, crumb) in crumbs.iter().enumerate() {
                        if index > 0 {
                            Icon { name: "chevron_right", size: IconSize::Sm, class: "df-browse-crumb-sep df-browse-crumb-sep-mirror" }
                        }
                        Button {
                            class: "df-browse-crumb",
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            "data-mirror-crumb": "true",
                            tabindex: "-1",
                            {crumb.label.clone()}
                        }
                    }
                    Icon { name: "chevron_right", size: IconSize::Sm, class: "df-browse-crumb-sep" }
                    IconButton {
                        class: "df-browse-crumb-overflow",
                        size: IconButtonSize::IconSm,
                        icon: "more_horiz",
                        label: "Show hidden folders".to_owned(),
                        "data-mirror-overflow": "true",
                        tabindex: "-1",
                    }
                }
            }
        }
        if menu_open {
            if let Some(position) = menu_pos {
                div {
                    id: CRUMB_MENU_ID,
                    class: "df-menu-enter df-browse-crumb-menu",
                    "data-testid": "browse-crumb-menu",
                    role: "menu",
                    "aria-labelledby": CRUMB_OVERFLOW_ID,
                    style: ctx_menu_surface_style(position.left, position.top, 0),
                    {menu_items.into_iter()}
                }
            }
        }
    }
}
