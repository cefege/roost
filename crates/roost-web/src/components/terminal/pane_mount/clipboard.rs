//! Terminal selection copies and their coordinator-history capture. Called by
//! the pane's reserved copy chord and copy-on-select; depends on its shared
//! pane state, the client's one clipboard write (`hooks::use_copy_clipboard`)
//! and the client RPC pump.

use super::PaneShared;

/// Copy a native terminal selection locally, then capture it after clipboard success.
pub(super) fn copy_selection(shared: &PaneShared) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let text = window
        .get_selection()
        .ok()
        .flatten()
        .and_then(|selection| selection.to_string().as_string())
        .unwrap_or_default();
    if text.is_empty() {
        return;
    }
    let session_id = shared.session_id.clone();
    let pump = shared.pump.clone();
    let history_text = text.clone();
    crate::components::hooks::use_copy_clipboard::copy_text_then(&text, move |accepted| {
        if accepted {
            add_clipboard_history_entry(&pump, &session_id, history_text, "selection");
        }
    });
}

fn add_clipboard_history_entry(
    pump: &crate::pump::Pump,
    session_id: &str,
    text: String,
    source_kind: &'static str,
) {
    use roost_client_core::client::rpc::calls::clipboard::ClipboardAdd;
    let call = ClipboardAdd {
        text,
        session_id: session_id.to_owned(),
        source_kind: source_kind.to_owned(),
    };
    let rpc = pump.rpc();
    let session_id = session_id.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = rpc.call(&call).await {
            tracing::debug!(target: "clipboard", %session_id, %error, "terminal copy history add failed");
        }
    });
}
