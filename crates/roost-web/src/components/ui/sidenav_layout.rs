//! Sidenav layout containers: the inset and inner panels, header and footer, the
//! scrolling content region, groups and labels, the menu list structure, and the
//! search input. Pure containers with `data-name` markers and no state of their own.
//! Consumed by `sidenav.rs` (the inner panel) and by callers composing a sidenav.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn SidenavInset(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] data_variant: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "bg-background relative flex w-full flex-1 flex-col data-[variant=Inset]:rounded-lg data-[variant=Inset]:border data-[variant=Inset]:border-sidenav-border data-[variant=Inset]:shadow-sm data-[variant=Inset]:m-2",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { "data-name": "SidenavInset", "data-variant": data_variant, class: "{merged_class}", {children} }
    }
}

#[component]
pub fn SidenavInner(
    #[props(into, optional)] variant: Option<String>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "flex flex-col w-full h-full bg-sidenav data-[variant=Floating]:rounded-lg data-[variant=Floating]:border data-[variant=Floating]:border-sidenav-border data-[variant=Floating]:shadow-sm",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "SidenavInner",
            "data-variant": variant.as_deref().unwrap_or(""),
            class: "{merged_class}",
            {children}
        }
    }
}

#[component]
pub fn SidenavHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!("flex flex-col gap-2 p-2", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "SidenavHeader", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavMenu(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "flex flex-col gap-1 w-full min-w-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { ul { "data-name": "SidenavMenu", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavMenuSub(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "border-sidenav-border mx-3.5 flex min-w-0 translate-x-px flex-col gap-1 border-l px-2.5 py-0.5 group-data-[collapsible=Icon]:hidden",
        class.as_deref().unwrap_or("")
    );
    rsx! { ul { "data-name": "SidenavMenuSub", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavMenuItem(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!("relative group/menu-item", class.as_deref().unwrap_or(""));
    rsx! { li { "data-name": "SidenavMenuItem", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavMenuSubItem(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!("group/menu-sub-item", class.as_deref().unwrap_or(""));
    rsx! { li { "data-name": "SidenavMenuSubItem", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavContent(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "scrollbar__on_hover flex min-h-0 flex-1 flex-col gap-2 overflow-auto group-data-[collapsible=Icon]:overflow-hidden",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "SidenavContent", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] data_sidenav: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "flex relative flex-col p-2 w-full min-w-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "SidenavGroup", "data-sidenav": data_sidenav, class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavGroupContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] data_sidenav: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!("w-full text-sm", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "SidenavGroupContent", "data-sidenav": data_sidenav, class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavGroupLabel(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-sidenav-foreground/70 ring-sidenav-ring flex h-8 shrink-0 items-center rounded-md px-2 text-xs font-medium outline-hidden transition-[margin,opacity] duration-200 ease-linear focus-visible:ring-2 [&>svg]:size-4 [&>svg]:shrink-0 group-data-[collapsible=Icon]:-mt-8 group-data-[collapsible=Icon]:opacity-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "SidenavGroupLabel", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!("flex flex-col gap-2 p-2", class.as_deref().unwrap_or(""));
    rsx! { footer { "data-name": "SidenavFooter", class: "{merged_class}", {children} } }
}

#[component]
pub fn SidenavInput(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] name: Option<String>,
    #[props(into, optional)] value: Option<String>,
    #[props(optional)] oninput: Option<EventHandler<FormEvent>>,
) -> Element {
    let merged_class = tw_merge!(
        "file:text-foreground placeholder:text-muted-foreground selection:bg-primary selection:text-primary-foreground dark:bg-input/30 border-input flex h-9 w-full min-w-0 rounded-md border bg-transparent px-3 py-1 text-base shadow-xs transition-[color,box-shadow] outline-none file:inline-flex file:h-7 file:border-0 file:bg-transparent file:text-sm file:font-medium disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 md:text-sm focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive read-only:bg-muted w-full h-8 shadow-none bg-background",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        input {
            "data-name": "SidenavInput",
            class: "{merged_class}",
            placeholder: placeholder,
            id: id,
            name: name,
            value: value,
            oninput: move |event| {
                if let Some(handler) = &oninput {
                    handler.call(event);
                }
            },
        }
    }
}
