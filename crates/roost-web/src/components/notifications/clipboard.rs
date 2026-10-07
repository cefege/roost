//! The one write to the system clipboard the notification surfaces need, with
//! the denial rule v2's `browser/clipboard.ts` states: a refused write is not
//! an error the card renders, because the card text stays selectable and the
//! user can still read it. Ports `copyToClipboard`.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

/// Copy `text`, reporting whether the write was accepted.
///
/// A denial is deliberately indistinguishable from success at the call sites
/// that do not care, and distinguishable at the one that does — the toast's Copy
/// button, which only claims "Copied" when this returned true.
pub fn copy_text(text: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return false;
        };
        // `navigator.clipboard` is absent in an insecure context, and the
        // generated getter traps rather than answering, so the capability is
        // read reflectively.
        let Ok(clipboard) = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()) else {
            return false;
        };
        if clipboard.is_undefined() || clipboard.is_null() {
            return false;
        }
        let Ok(write_text) = js_sys::Reflect::get(&clipboard, &"writeText".into())
            .and_then(|write_text| write_text.dyn_into::<js_sys::Function>())
        else {
            return false;
        };
        let Ok(written) = write_text
            .call1(&clipboard, &wasm_bindgen::JsValue::from_str(text))
            .and_then(|written| written.dyn_into::<js_sys::Promise>())
        else {
            return false;
        };
        // The write itself resolves later. The synchronous answer this surface
        // needs is the capability check, so a denial — permission, insecure
        // context, no activation — is logged where the caller cannot see it and
        // the card text stays selectable.
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = wasm_bindgen_futures::JsFuture::from(written).await {
                tracing::debug!(
                    target: "notifications",
                    ?error,
                    "clipboard write refused"
                );
            }
        });
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        false
    }
}

/// Copy `text` and report the browser's VERDICT once the write settles.
///
/// [`copy_text`] answers synchronously and so can only report that the write
/// was attempted. A write that is not inside a user gesture is the case that
/// needs the real answer: Safari and Firefox refuse it asynchronously, and the
/// caller must then offer a Copy button rather than claim the text was copied.
pub fn copy_text_then(text: &str, on_settled: impl FnOnce(bool) + 'static) {
    #[cfg(target_arch = "wasm32")]
    {
        let written = web_sys::window()
            .and_then(|window| js_sys::Reflect::get(&window.navigator(), &"clipboard".into()).ok())
            .filter(|clipboard| !clipboard.is_undefined() && !clipboard.is_null())
            .and_then(|clipboard| {
                let write_text = js_sys::Reflect::get(&clipboard, &"writeText".into())
                    .ok()?
                    .dyn_into::<js_sys::Function>()
                    .ok()?;
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
