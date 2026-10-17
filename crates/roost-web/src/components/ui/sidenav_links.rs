//! Sidenav navigation controls: links, menu buttons (a link or a button), sub-links,
//! the hover-revealed menu action, and the ellipsis trigger. Active state is passed
//! in by the caller, so these components never read the browser location.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::sidenav_types::{SidenavMenuButtonSize, SidenavMenuButtonVariant};

#[component]
pub fn SidenavLink(
    #[props(into)] href: String,
    #[props(into, optional)] class: Option<String>,
    #[props(default = false)] active: bool,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "peer/menu-button flex w-full items-center gap-2 overflow-hidden rounded-md p-2 text-left outline-hidden ring-sidenav-ring transition-[width,height,padding] focus-visible:ring-2 active:bg-sidenav-accent active:text-sidenav-accent-foreground disabled:pointer-events-none disabled:opacity-50 group-has-data-[sidenav=menu-action]/menu-item:pr-8 aria-disabled:pointer-events-none aria-disabled:opacity-50 aria-[current=page]:bg-sidenav-accent aria-[current=page]:font-semibold aria-[current=page]:text-sidenav-accent-foreground data-[state=open]:hover:bg-sidenav-accent data-[state=open]:hover:text-sidenav-accent-foreground group-data-[collapsible=Icon]:size-8! group-data-[collapsible=Icon]:p-2! [&>span:last-child]:truncate [&>svg]:size-4 [&>svg]:shrink-0 hover:bg-sidenav-accent hover:text-sidenav-accent-foreground h-8 text-sm",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        a {
            "data-name": "SidenavLink",
            class: "{merged_class}",
            href: "{href}",
            "aria-current": if active { "page" } else { "false" },
            {children}
        }
    }
}

#[component]
pub fn SidenavMenuButton(
    #[props(into, optional)] href: Option<String>,
    #[props(into, optional)] class: Option<String>,
    #[props(default = SidenavMenuButtonVariant::default())] variant: SidenavMenuButtonVariant,
    #[props(default = SidenavMenuButtonSize::default())] size: SidenavMenuButtonSize,
    #[props(into, optional)] aria_current: Option<String>,
    children: Element,
) -> Element {
    let base = "peer/menu-button flex w-full items-center gap-2 overflow-hidden rounded-md p-2 text-left text-sm outline-hidden ring-sidenav-ring transition-[width,height,padding] hover:bg-sidenav-accent hover:text-sidenav-accent-foreground focus-visible:ring-2 active:bg-sidenav-accent active:text-sidenav-accent-foreground disabled:pointer-events-none disabled:opacity-50 group-has-data-[sidenav=menu-action]/menu-item:pr-8 aria-disabled:pointer-events-none aria-disabled:opacity-50 aria-[current=page]:bg-sidenav-accent aria-[current=page]:font-medium aria-[current=page]:text-sidenav-accent-foreground data-[state=open]:hover:bg-sidenav-accent data-[state=open]:hover:text-sidenav-accent-foreground [&>span:last-child]:truncate [&>svg]:size-4 [&>svg]:shrink-0 group-data-[collapsible=Icon]:size-8! group-data-[collapsible=Icon]:p-0! [&>svg]:stroke-2 aria-[current=page]:[&>svg]:stroke-[2.7]";
    let variant_class = match variant {
        SidenavMenuButtonVariant::Default => {
            "hover:bg-sidenav-accent hover:text-sidenav-accent-foreground"
        }
        SidenavMenuButtonVariant::Outline => {
            "bg-background ring-1 ring-sidenav-border hover:bg-sidenav-accent hover:text-sidenav-accent-foreground hover:ring-sidenav-accent"
        }
    };
    let size_class = match size {
        SidenavMenuButtonSize::Default => "h-8 text-sm",
        SidenavMenuButtonSize::Sm => "h-7 text-xs",
        SidenavMenuButtonSize::Lg => "h-12",
    };
    let merged_class = tw_merge!(
        base,
        variant_class,
        size_class,
        class.as_deref().unwrap_or("")
    );
    let data_variant = match variant {
        SidenavMenuButtonVariant::Default => "Default",
        SidenavMenuButtonVariant::Outline => "Outline",
    };
    let data_size = match size {
        SidenavMenuButtonSize::Default => "default",
        SidenavMenuButtonSize::Sm => "sm",
        SidenavMenuButtonSize::Lg => "lg",
    };

    if let Some(href) = href {
        rsx! {
            a {
                "data-name": "SidenavMenuButton",
                "data-variant": data_variant,
                "data-size": data_size,
                class: "{merged_class}",
                href: "{href}",
                "aria-current": aria_current.unwrap_or_else(|| "false".to_string()),
                {children}
            }
        }
    } else {
        rsx! {
            button {
                "data-name": "SidenavMenuButton",
                "data-variant": data_variant,
                "data-size": data_size,
                class: "{merged_class}",
                {children}
            }
        }
    }
}

#[component]
pub fn SidenavMenuSubButton(
    #[props(into)] href: String,
    #[props(into, optional)] class: Option<String>,
    #[props(default = false)] active: bool,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-sidenav-foreground ring-sidenav-ring hover:bg-sidenav-accent hover:text-sidenav-accent-foreground active:bg-sidenav-accent active:text-sidenav-accent-foreground flex h-7 min-w-0 -translate-x-px items-center gap-2 overflow-hidden rounded-md px-2 text-sm outline-hidden focus-visible:ring-2 disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50 aria-[current=page]:bg-sidenav-accent aria-[current=page]:font-medium aria-[current=page]:text-sidenav-accent-foreground [&>svg]:size-4 [&>svg]:shrink-0",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        a {
            "data-name": "SidenavMenuSubButton",
            class: "{merged_class}",
            href: "{href}",
            "aria-current": if active { "page" } else { "false" },
            {children}
        }
    }
}

#[component]
pub fn SidenavMenuAction(
    #[props(default = false)] show_on_hover: bool,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-sidenav-foreground ring-sidenav-ring hover:bg-sidenav-accent hover:text-sidenav-accent-foreground peer-hover/menu-button:text-sidenav-accent-foreground absolute top-1.5 right-1 flex aspect-square w-5 items-center justify-center rounded-md p-0 outline-hidden transition-transform focus-visible:ring-2 [&>svg]:size-4 [&>svg]:shrink-0 after:absolute after:-inset-2 md:after:hidden peer-data-[size=sm]/menu-button:top-1 peer-data-[size=default]/menu-button:top-1.5 peer-data-[size=lg]/menu-button:top-2.5 group-data-[collapsible=Icon]:hidden",
        if show_on_hover {
            "group-focus-within/menu-item:opacity-100 group-hover/menu-item:opacity-100 data-[state=open]:opacity-100 peer-data-[active=true]/menu-button:text-sidenav-accent-foreground md:opacity-0"
        } else {
            ""
        },
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-sidenav": "menu-action",
            "data-name": "SidenavMenuAction",
            class: "{merged_class}",
            {children}
        }
    }
}

#[component]
pub fn DropdownMenuTriggerEllipsis(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-sidenav-foreground ring-sidenav-ring hover:bg-sidenav-accent hover:text-sidenav-accent-foreground peer-hover/menu-button:text-sidenav-accent-foreground absolute top-1.5 right-1 flex aspect-square w-5 items-center justify-center rounded-md p-0 outline-hidden transition-transform focus-visible:ring-2 [&>svg]:size-4 [&>svg]:shrink-0 after:absolute after:-inset-2 md:after:hidden peer-data-[size=sm]/menu-button:top-1 peer-data-[size=default]/menu-button:top-1.5 peer-data-[size=lg]/menu-button:top-2.5 group-data-[collapsible=Icon]:hidden peer-data-[active=true]/menu-button:text-sidenav-accent-foreground group-focus-within/menu-item:opacity-100 group-hover/menu-item:opacity-100 data-[state=open]:opacity-100 md:opacity-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { button { "data-name": "DropdownMenuTriggerEllipsis", class: "{merged_class}", {children} } }
}
