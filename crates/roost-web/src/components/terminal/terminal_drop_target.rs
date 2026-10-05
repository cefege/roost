//! A terminal pane as a drop target: the hook `CellTerminal` calls to take
//! files and links dropped on it for as long as it is mounted, and the overlay
//! that shows where a dragged file will land. The listeners and rules are
//! `terminal_chrome::file_drop_dom` and `terminal_chrome::file_drop`; the
//! upload is whatever the pane's attach button runs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;

use super::dom::{now_ms, sleep_ms};
use super::pane_handle::PaneHandle;
use super::pane_state::PaneFlags;
use crate::components::md::{Icon, IconSize, Surface, SurfaceRadius};
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::terminal_chrome::file_drop_dom::{
    PaneDropTarget, install_pane_drop_listeners,
};

/// How long a file drag may go without a `dragover` before its overlay is
/// taken down. A drag over the page repeats `dragover` at most every 550 ms
/// even when the pointer is still; one that stopped was cancelled without the
/// `dragleave` that would otherwise have cleared it.
const DRAG_STALE_MS: u64 = 1_000;

/// Take drops for this pane until it unmounts, and report whether a file drag
/// it would take is over the page.
///
/// `on_files` is the attach button's handler, so a drop and a pick upload
/// alike. A dropped link is pasted through the pane's paste guard. Everything
/// is captured on the first render: a pane is mounted for one session.
pub fn use_terminal_file_drop(
    session_id: &str,
    flags: Rc<Cell<PaneFlags>>,
    handle: PaneHandle,
    on_files: impl FnMut(Vec<ChosenFile>) + 'static,
) -> bool {
    let hover = use_signal(|| false);
    let last_over_ms = use_hook(|| Rc::new(Cell::new(0_u64)));
    // Taken in `use_drop`, so the document listeners go when the pane unmounts
    // rather than whenever the last clone of the hook value happens to drop.
    let listeners = use_hook(|| {
        let last_over_ms = Rc::clone(&last_over_ms);
        let target = PaneDropTarget {
            session_id: session_id.to_owned(),
            focused: Rc::new(move || {
                let flags = flags.get();
                flags.focused && flags.surface_visible
            }),
            on_files: EventHandler::new(on_files),
            on_link: Rc::new(move |text: &str| handle.paste_text(text)),
            on_hover: Rc::new(move |over: bool| {
                let mut hover = hover;
                if over {
                    last_over_ms.set(now_ms());
                }
                if *hover.peek() != over {
                    hover.set(over);
                }
            }),
        };
        Rc::new(RefCell::new(Some(install_pane_drop_listeners(target))))
    });
    use_drop(move || drop(listeners.borrow_mut().take()));
    use_effect(move || {
        if !hover() {
            return;
        }
        let last_over_ms = Rc::clone(&last_over_ms);
        let mut hover = hover;
        spawn(async move {
            while *hover.peek() {
                sleep_ms(DRAG_STALE_MS).await;
                if now_ms().saturating_sub(last_over_ms.get()) >= DRAG_STALE_MS {
                    hover.set(false);
                }
            }
        });
    });
    hover()
}

/// The drop affordance over a pane a file drag is aimed at. Click-through, so
/// the drop itself still lands on the pane beneath it.
#[component]
pub fn TerminalDropOverlay(active: bool) -> Element {
    if !active {
        return rsx! {};
    }
    rsx! {
        div {
            "data-testid": "terminal-drop-overlay",
            "aria-hidden": "true",
            style: "position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; padding: var(--md-space-6); pointer-events: none; z-index: 6; background: color-mix(in srgb, var(--md-scrim) 35%, transparent); outline: 2px dashed var(--md-primary); outline-offset: calc(var(--md-space-2) * -1);",
            Surface {
                level: 2,
                elevation: 3,
                radius: SurfaceRadius::Lg,
                pad: 5,
                border: true,
                style: "display: flex; align-items: center; gap: var(--md-space-3); color: var(--md-sys-color-on-surface);",
                Icon { name: "upload", size: IconSize::Lg, style: "color: var(--md-primary);" }
                span { class: "md-title-s", "Drop to upload to this terminal" }
            }
        }
    }
}
