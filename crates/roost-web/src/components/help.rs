//! The `/help` route: the keybindings page, and the Shift+? overlay host that
//! `HelpSurface` owns so one mount covers both. Ports
//! `apps/web/src/components/palette/Help.tsx` (the page) and the mounting of
//! `HelpOverlay` from `App.tsx`; the overlay itself is `help::overlay` and both
//! read the catalogue in `help::shortcuts`.
//!
//! The page is not a second copy of the shortcut list: every row is a
//! `ShortcutEntry`, so a binding cannot be right on one surface and wrong on
//! the other. Escape leaves the page the way it arrived, through the history
//! entry `navigate(-1)` used, because the shell's `popstate` listener repaints
//! the rendered path from it.

pub mod overlay;
pub mod shortcuts;

use dioxus::prelude::*;
use libero::components::Box;
use libero::sx::sx;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;

use self::overlay::HelpOverlayHost;
use self::shortcuts::{SHORTCUTS, reader_platform};

/// The help route: the keybindings page plus the Shift+? overlay host.
#[component]
pub fn HelpSurface() -> Element {
    install_escape_back();
    let platform = reader_platform();
    let rows: Vec<(String, &'static str)> = SHORTCUTS
        .iter()
        .map(|entry| (entry.binding_label(platform), entry.label))
        .collect();
    let table_sx = use_hook(|| sx().border_collapse("collapse"));
    let row_sx = use_hook(|| sx().border_color("var(--border)"));
    let binding_cell_sx = use_hook(|| {
        sx().width("40%")
            .padding_inline_start("0")
            .padding_inline_end("md")
            .padding_block("sm")
            .font_family("monospace")
            .color("var(--workbench-active)")
    });
    let label_cell_sx = use_hook(|| sx().padding_inline("0").padding_block("sm"));
    let page_sx = use_hook(|| {
        sx().box_sizing("border-box")
            .display("flex")
            .height("100%")
            .min_height("0")
            .width("100%")
            .flex_direction("column")
            .overflow("auto")
            .background("surface")
            .padding("lg")
            .color("var(--foreground)")
    });
    let content_sx = use_hook(|| sx().margin_inline("auto").max_width("32rem"));
    let title_sx = use_hook(|| {
        sx().margin_top("0")
            .margin_bottom("1.25rem")
            .font_size("xl")
    });
    let hint_sx = use_hook(|| {
        sx().margin_top("1.25rem")
            .margin_bottom("0")
            .font_size("xs")
            .color("var(--muted-foreground)")
    });
    rsx! {
        Box { component: "div", sx: page_sx.clone(),
            Box { component: "div", sx: content_sx.clone(),
                Box { component: "h2", sx: title_sx.clone(), "Help / Keybindings" }
                Box { component: "table", sx: table_sx.clone(),
                    Box { component: "tbody",
                        for (binding, label) in rows {
                            Box { component: "tr", sx: row_sx.clone(),
                                Box { component: "td", sx: binding_cell_sx.clone(), {binding} }
                                Box { component: "td", sx: label_cell_sx.clone(), {label} }
                            }
                        }
                    }
                }
                Box { component: "p", sx: hint_sx.clone(), "Press Esc to close." }
            }
            HelpOverlayHost {}
        }
    }
}

/// The page's own Escape handler, for the lifetime of the route.
#[cfg(target_arch = "wasm32")]
fn install_escape_back() {
    use wasm_bindgen::JsCast as _;

    use_hook(|| {
        let callback =
            Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|event: web_sys::KeyboardEvent| {
                if event.key() == "Escape" {
                    go_back();
                }
            });
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref());
        }
        Rc::new(EscapeBackListener {
            callback: Some(callback),
        })
    });
}

/// A native build has no document to listen on.
#[cfg(not(target_arch = "wasm32"))]
fn install_escape_back() {}

/// Drops the page's Escape listener: a route change must not leave one behind
/// that navigates away from a page the reader is no longer on. Held behind an
/// `Rc` because the hook that owns it hands out a clone on every render.
#[cfg(target_arch = "wasm32")]
struct EscapeBackListener {
    callback: Option<Closure<dyn FnMut(web_sys::KeyboardEvent)>>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for EscapeBackListener {
    fn drop(&mut self) {
        use wasm_bindgen::JsCast as _;

        if let (Some(window), Some(callback)) = (web_sys::window(), self.callback.as_ref()) {
            let _ = window
                .remove_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref());
        }
    }
}

/// Leave the page the way it was entered, as `navigate(-1)` did.
///
/// A browser that opened `/help` directly has no entry to go back to, and v2
/// left the reader where they were in that case too.
#[cfg(target_arch = "wasm32")]
fn go_back() {
    let moved = web_sys::window()
        .and_then(|window| window.history().ok())
        .is_some_and(|history| history.back().is_ok());
    if !moved {
        tracing::warn!(target: "help", "the browser refused history.back");
    }
}
