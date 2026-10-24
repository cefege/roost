//! The sidenav root: `SidenavWrapper` owns the open flag and the `--sidenav-width`
//! variable, `Sidenav` renders the responsive aside, and `SidenavTrigger` plus the
//! rail toggle the flag. Callers set the width through the wrapper's `style`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::sidenav_layout::SidenavInner;
use super::sidenav_types::{
    SidenavCollapsible, SidenavContext, SidenavSide, SidenavState, SidenavVariant,
};

#[component]
pub fn SidenavWrapper(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(default = true)] default_open: bool,
    #[props(default = 160)] min_width: u32,
    #[props(default = 480)] max_width: u32,
    children: Element,
) -> Element {
    let open = use_signal(|| default_open);
    provide_context(SidenavContext { open });
    let merged_class = tw_merge!(
        "group/sidenav-wrapper has-data-[variant=Inset]:bg-sidenav flex h-full w-full",
        class.as_deref().unwrap_or("")
    );
    let wrapper_style = format!("--sidenav-width: 256px;{}", style.as_deref().unwrap_or(""));
    rsx! {
        div {
            "data-name": "SidenavWrapper",
            "data-min-width": "{min_width}",
            "data-max-width": "{max_width}",
            style: "{wrapper_style}",
            class: "{merged_class}",
            {children}
        }
    }
}

#[component]
pub fn Sidenav(
    #[props(into, optional)] class: Option<String>,
    #[props(default = SidenavVariant::default())] variant: SidenavVariant,
    #[props(default = SidenavState::default())] data_state: SidenavState,
    #[props(default = SidenavSide::default())] data_side: SidenavSide,
    #[props(default = SidenavCollapsible::default())] data_collapsible: SidenavCollapsible,
    children: Element,
) -> Element {
    let fallback_open = use_signal(|| data_state == SidenavState::Expanded);
    let is_open = try_use_context::<SidenavContext>().map_or(fallback_open, |context| context.open);
    let class_value = class.as_deref().unwrap_or("");

    if data_collapsible == SidenavCollapsible::None {
        let merged_class = tw_merge!(
            "flex flex-col h-full bg-sidenav text-sidenav-foreground w-(--sidenav-width)",
            class_value
        );
        return rsx! {
            aside { "data-name": "Sidenav", class: "{merged_class}", {children} }
        };
    }

    let gap_class = tw_merge!(
        "relative w-(--sidenav-width) bg-transparent transition-[width] duration-200 ease-linear group-data-[collapsible=Offcanvas]:w-0 group-data-[side=Right]:rotate-180",
        match variant {
            SidenavVariant::Sidenav => "group-data-[collapsible=Icon]:w-(--sidenav-width-icon)",
            SidenavVariant::Floating | SidenavVariant::Inset => {
                "group-data-[collapsible=Icon]:w-[calc(var(--sidenav-width-icon)+(--spacing(4)))]"
            }
        }
    );
    let container_class = tw_merge!(
        "fixed inset-y-0 z-10 hidden h-svh w-(--sidenav-width) transition-[left,right,width] duration-200 ease-linear md:flex",
        class_value,
        match data_side {
            SidenavSide::Left =>
                "left-0 group-data-[collapsible=Offcanvas]:left-[calc(var(--sidenav-width)*-1)]",
            SidenavSide::Right =>
                "right-0 group-data-[collapsible=Offcanvas]:right-[calc(var(--sidenav-width)*-1)]",
        },
        match variant {
            SidenavVariant::Sidenav => {
                "group-data-[collapsible=Icon]:w-(--sidenav-width-icon) group-data-[side=Left]:border-r group-data-[side=Right]:border-l"
            }
            SidenavVariant::Floating | SidenavVariant::Inset => {
                "p-2 group-data-[collapsible=Icon]:w-[calc(var(--sidenav-width-icon)+(--spacing(4))+2px)]"
            }
        }
    );

    rsx! {
        aside {
            "data-name": "Sidenav",
            "data-sidenav": data_state.as_str(),
            "data-state": if is_open() { "Expanded" } else { "Collapsed" },
            "data-side": data_side.as_str(),
            class: "hidden md:block group peer text-sidenav-foreground data-[state=Collapsed]:hidden",
            div { "data-name": "SidenavGap", class: "{gap_class}" }
            div {
                "data-name": "SidenavContainer",
                class: "{container_class}",
                SidenavInner { variant: variant.as_str(), {children} }
                SidenavToggleRail {}
            }
        }
    }
}

#[component]
pub fn SidenavTrigger(children: Element) -> Element {
    let context = try_use_context::<SidenavContext>();
    rsx! {
        button {
            "data-name": "SidenavTrigger",
            class: "inline-flex gap-2 justify-center items-center -ml-1 text-sm font-medium whitespace-nowrap rounded-md transition-all outline-none disabled:opacity-50 disabled:pointer-events-none [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0 [&_svg]:shrink-0 aria-invalid:ring-destructive/20 aria-invalid:border-destructive size-7 dark:aria-invalid:ring-destructive/40 dark:hover:bg-accent/50 hover:bg-accent hover:text-accent-foreground focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px]",
            onclick: move |_| {
                if let Some(context) = context {
                    context.toggle();
                }
            },
            {children}
        }
    }
}

#[component]
fn SidenavToggleRail() -> Element {
    let context = try_use_context::<SidenavContext>();
    rsx! {
        button {
            "data-name": "SidenavToggleRail",
            "aria-label": "Toggle Sidenav",
            tabindex: "-1",
            class: "hidden absolute inset-y-0 z-20 w-4 transition-all ease-linear -translate-x-1/2 sm:flex group-data-[side=Left]:-right-4 group-data-[side=Right]:left-0 after:absolute after:inset-y-0 after:left-1/2 after:w-[2px] in-data-[side=Left]:cursor-w-resize in-data-[side=Right]:cursor-e-resize [[data-side=Left][data-state=Collapsed]_&]:cursor-e-resize [[data-side=Right][data-state=Collapsed]_&]:cursor-w-resize group-data-[collapsible=Offcanvas]:translate-x-0 group-data-[collapsible=Offcanvas]:after:left-full [[data-side=Left][data-collapsible=Offcanvas]_&]:-right-2 [[data-side=Right][data-collapsible=Offcanvas]_&]:-left-2 hover:after:bg-sidenav-border hover:group-data-[collapsible=Offcanvas]:bg-sidenav",
            onclick: move |_| {
                if let Some(context) = context {
                    context.toggle();
                }
            },
        }
    }
}
