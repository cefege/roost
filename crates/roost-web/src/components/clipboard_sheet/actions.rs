//! Clipboard history RPC actions and accepted local-copy feedback.
//! The persistent host supplies pumps and current-opening completion guards.
//! Coordinator failures leave confirmation state for the caller to display.

use crate::components::hooks::use_copy_clipboard::copy_text_then;
use crate::components::notifications::store_write::write_store;
use crate::components::terminal::dom::now_ms;
use crate::pump::Pump;
use roost_client_core::client::rpc::calls::clipboard::{
    ClipboardClear, ClipboardDelete, ClipboardList,
};
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

pub(super) fn copy_entry(pump: &Pump, text: &str) {
    let pump = pump.clone();
    copy_text_then(text, move |accepted| {
        if accepted {
            notify(&pump, "Copied", ToastKind::Ok);
        }
    });
}

pub(super) fn load_history(pump: Pump) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&ClipboardList).await {
            Ok(entries) => pump.write_store(|store| store.clipboard_history.replace(entries)),
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard history load failed");
                notify(
                    &pump,
                    "Could not load the clipboard history",
                    ToastKind::Err,
                );
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardList);
}

pub(super) fn delete_entry(pump: Pump, id: String) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&ClipboardDelete(id.clone())).await {
            Ok(_) => pump.write_store(|store| store.clipboard_history.remove(&id)),
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard delete failed");
                notify(&pump, "Could not delete the entry", ToastKind::Err);
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardDelete(id));
}

pub(super) fn clear_history(pump: Pump, on_settled: impl FnOnce(bool) + 'static) {
    #[cfg(target_arch = "wasm32")]
    dioxus::core::spawn_forever(async move {
        let accepted = match pump.rpc().call(&ClipboardClear).await {
            Ok(_) => {
                pump.write_store(|store| store.clipboard_history.replace(Vec::new()));
                true
            }
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard clear failed");
                notify(
                    &pump,
                    "Could not clear the clipboard history",
                    ToastKind::Err,
                );
                false
            }
        };
        on_settled(accepted);
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardClear, on_settled);
}

pub(super) fn notify(pump: &Pump, message: &str, kind: ToastKind) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "clipboard-history",
        },
        "clipboard-history",
    );
    write_store(pump, |store| {
        add_toast(
            store,
            id,
            message,
            kind,
            ToastOptions::with_ttl(Some(2_500)),
            now_ms(),
        );
    });
}
