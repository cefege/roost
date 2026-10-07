//! The browser end of OSC 52: put a terminal's clipboard write on the system
//! clipboard, but only in the focused tab that is showing that session.
//!
//! Mounted once by `NotificationDock`; drains `Store::terminal_clipboard_requests`
//! after each store revision. A write the browser refuses (no user gesture, as
//! Safari and Firefox require) becomes a card whose Copy button supplies one.

use dioxus::prelude::*;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

use super::agent_notifications::agent_attention;
use super::clipboard::copy_text_then;
use super::store_write::write_store;
use crate::components::terminal::dom::now_ms;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;

/// How much of a refused write the card previews. The Copy button carries the
/// whole text; the card only has to let the operator recognise it.
const MANUAL_COPY_PREVIEW_CHARS: usize = 160;

/// Drains OSC 52 requests after each client-store change.
#[component]
pub fn TerminalClipboard() -> Element {
    let pump = use_store();
    let path = use_location();
    let attention = agent_attention::use_page_attention();

    use_effect(move || {
        let _ = pump.revision().read();
        let attended = *attention.read();
        let requests = {
            let core = pump.core();
            let mut core = core.borrow_mut();
            core.store_mut()
                .terminal_clipboard_requests
                .drain()
                .collect::<Vec<_>>()
        };
        if requests.is_empty() {
            return;
        }
        // Every open tab receives the same write; only the one the operator is
        // looking at may take the clipboard, or two tabs would race for it.
        let visible_session = attended
            .then(|| {
                let core = pump.core();
                let core = core.borrow();
                active_session_for_path(core.store(), &BrowserWorkerPaths, &path())
                    .map(|session| session.id.as_str().to_owned())
            })
            .flatten();
        for request in requests {
            if visible_session.as_deref() != Some(request.session_id.as_str()) {
                tracing::debug!(
                    session_id = %request.session_id,
                    attended,
                    "terminal clipboard write left to the tab showing its session"
                );
                continue;
            }
            let session_id = request.session_id;
            let text = request.text;
            let fallback_text = text.clone();
            let pump = pump.clone();
            copy_text_then(&text, move |accepted| {
                if accepted {
                    raise_copied_toast(&pump, &session_id);
                } else {
                    tracing::info!(
                        session_id = %session_id,
                        "browser refused a terminal clipboard write; offering a Copy button"
                    );
                    raise_manual_copy_toast(
                        &pump,
                        &session_id,
                        "The terminal copied text. Click Copy to put it on your clipboard.",
                        fallback_text,
                    );
                }
            });
        }
    });

    rsx! {}
}

/// The short confirmation shown after the terminal's text reached the clipboard,
/// whether written directly or through the card's Copy button.
pub fn raise_copied_toast(pump: &Pump, session_id: &str) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "terminal-clipboard",
        },
        session_id,
    );
    write_store(pump, |store| {
        add_toast(
            store,
            id,
            "Copied from terminal",
            ToastKind::Ok,
            ToastOptions::with_ttl(Some(2_500)),
            now_ms(),
        );
    });
}

/// The card for a refused write: a preview, and a Copy button whose click is
/// the user gesture the browser asked for. One per session, newest wins.
/// Also used by "Copy last command output", whose write lands after an await
/// and so outside the palette press that asked for it.
pub fn raise_manual_copy_toast(pump: &Pump, session_id: &str, message: &str, text: String) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "terminal-clipboard-manual",
        },
        session_id,
    );
    let mut preview: String = text.chars().take(MANUAL_COPY_PREVIEW_CHARS).collect();
    if preview.len() < text.len() {
        preview.push('…');
    }
    let options = ToastOptions::with_ttl(None)
        .with_details(preview)
        .with_copy_action(text);
    write_store(pump, |store| {
        add_toast(store, id, message, ToastKind::Warn, options, now_ms());
    });
}
