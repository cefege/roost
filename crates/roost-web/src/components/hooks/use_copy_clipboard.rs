//! rust-ui's `use_copy_clipboard`, and the one clipboard write in the client:
//! a component's Copy button takes the hook and its `copied` flag; a reader
//! with no component to hold one (a terminal selection, a delegated code
//! block, a menu item) calls [`copy_text_then`]. Unlike the registry source,
//! `copied` rises only once the browser has accepted the write, and its timer
//! never clears a newer copy's flag or writes a dropped signal.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

const DEFAULT_TIMEOUT_MS: i32 = 2000;

/// A copy function and whether its last copy is still being shown, for
/// `timeout_ms` (two seconds when `None`). The flag rises when the browser
/// accepts the write; a refused write leaves it down.
pub fn use_copy_clipboard(timeout_ms: Option<i32>) -> (impl Fn(&str) + Clone, ReadSignal<bool>) {
    let copied = use_signal(|| false);
    let latest = use_hook(|| CopyValue::new(0_u64));
    // Converted once: every `From<Signal>` allocates a box the scope keeps.
    let copied_read = use_hook(move || ReadSignal::from(copied));
    let timeout = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);

    let copy_fn = move |text: &str| {
        copy_text_then(text, move |accepted| {
            if accepted {
                show_copied(timeout, latest, copied);
            }
        });
    };

    (copy_fn, copied_read)
}

/// Raise `copied` for `timeout_ms`. The verdict can land after the component
/// unmounted, so every write here tolerates a dropped signal.
fn show_copied(timeout_ms: i32, latest: CopyValue<u64>, copied: Signal<bool>) {
    let (mut latest, mut copied) = (latest, copied);
    let Ok(ticket) = latest.try_write().map(|mut latest| {
        *latest += 1;
        *latest
    }) else {
        return;
    };
    if let Ok(mut flag) = copied.try_write() {
        *flag = true;
    }
    clear_after(timeout_ms, ticket, latest, copied);
}

/// Lower `copied` once `timeout_ms` has passed, unless a newer copy owns it
/// by then or its component is gone.
#[cfg(target_arch = "wasm32")]
fn clear_after(timeout_ms: i32, ticket: u64, latest: CopyValue<u64>, copied: Signal<bool>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let clear = wasm_bindgen::closure::Closure::once_into_js(move || {
        let mut copied = copied;
        if latest.try_read().is_ok_and(|latest| *latest == ticket)
            && let Ok(mut flag) = copied.try_write()
        {
            *flag = false;
        }
    });
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(clear.unchecked_ref(), timeout_ms);
}

#[cfg(not(target_arch = "wasm32"))]
fn clear_after(timeout_ms: i32, ticket: u64, latest: CopyValue<u64>, copied: Signal<bool>) {
    let _ = (timeout_ms, ticket, latest, copied);
}

/// Copy `text` and report the browser's verdict once the write settles:
/// `true` only for a write the browser accepted. A refusal — no clipboard in
/// an insecure context, no permission, no user activation (Safari and Firefox
/// refuse a write outside a gesture asynchronously) — reports `false`, and the
/// caller then offers a Copy button rather than claim the text was copied.
pub fn copy_text_then(text: &str, on_settled: impl FnOnce(bool) + 'static) {
    #[cfg(target_arch = "wasm32")]
    {
        let written = clipboard_writer().and_then(|(clipboard, write_text)| {
            write_text
                .call1(&clipboard, &wasm_bindgen::JsValue::from_str(text))
                .ok()?
                .dyn_into::<js_sys::Promise>()
                .ok()
        });
        let Some(written) = written else {
            on_settled(false);
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let accepted = wasm_bindgen_futures::JsFuture::from(written).await.is_ok();
            on_settled(accepted);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        on_settled(false);
    }
}

/// `navigator.clipboard` and its `writeText`, read reflectively: the clipboard
/// is absent in an insecure context, and the generated getter traps there
/// rather than answering.
#[cfg(target_arch = "wasm32")]
fn clipboard_writer() -> Option<(wasm_bindgen::JsValue, js_sys::Function)> {
    let window = web_sys::window()?;
    let clipboard = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()).ok()?;
    if clipboard.is_undefined() || clipboard.is_null() {
        return None;
    }
    let write_text = js_sys::Reflect::get(&clipboard, &"writeText".into())
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    Some((clipboard, write_text))
}
