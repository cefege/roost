//! Real Libero modal, drawer, menu and floating-interaction examples.

use dioxus::prelude::*;
use libero::{
    components::{Button, HoverCard, Menu, MenuEntry, MenuItem, Text, Title, Tooltip, use_menu},
    hooks::{DrawerOptions, ModalScope, use_drawer, use_modal},
};

use super::dialog_specimen::DialogSpecimen;
use super::popover_specimen::PopoverSpecimen;

#[component]
pub fn DesignOverlayStates() -> Element {
    let dialog = use_modal(|scope: ModalScope<()>| {
        rsx! {
            DialogSpecimen { on_close: move |_| scope.close() }
        }
    });
    let drawer = use_drawer(
        DrawerOptions {
            anchor: "end".into(),
            size: "sm".into(),
            title: Some("Workspace details".into()),
            close_button: true,
            ..Default::default()
        },
        |scope: ModalScope<()>| {
            rsx! {
                Text { "Drawer content uses the same modal lifecycle and focus policy." }
                Button { onclick: move |_| scope.close(), "Done" }
            }
        },
    );
    let menu = use_menu();
    let mut chosen = use_signal(|| "No command selected".to_string());
    let menu_items = vec![
        MenuEntry::Group {
            label: "Session".into(),
            items: vec![
                MenuItem::new("Open session")
                    .onselect(move |_| chosen.set("Open session".into()))
                    .into(),
                MenuItem::new("Copy session link")
                    .onselect(move |_| chosen.set("Copy session link".into()))
                    .into(),
            ],
        },
        MenuEntry::Separator,
        MenuItem::new("Unavailable action")
            .disabled(true)
            .description("This action is unavailable in the specimen")
            .into(),
    ];
    rsx! {
        Title { order: 2, size: "lg", "Overlays and menus" }
        Button { onclick: move |_| { dialog.open(); }, "Open dialog" }
        Button { variant: "outlined", onclick: move |_| { drawer.open(); }, "Open drawer" }
        Menu { state: menu, items: menu_items,
            Button { variant: "outlined", attributes: menu.a11y_attributes(), "Session actions" }
        }
        Text { role: "status", "Last menu action: {chosen()}" }
        Tooltip { label: rsx! { "Saves the current draft" }, label_id: "design-save-tip",
            Button { variant: "outlined", aria_describedby: "design-save-tip", "Save draft" }
        }
        HoverCard { aria_label: "Worker details",
            content: rsx! { Text { "ovh1 · connected · coordinator link healthy" } },
            Button { variant: "outlined", "Worker details" }
        }
        PopoverSpecimen {}
    }
}
