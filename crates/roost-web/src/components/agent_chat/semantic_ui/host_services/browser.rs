//! Browser primitives behind OMP host-service prompts.
//!
//! A new tab is opened synchronously under the operator's gesture and kept
//! about:blank until the worker admits the reply; only then is the validated
//! address put in it. The opener is severed before any navigation so the
//! destination cannot reach this document.

/// A tab opened by `open_blank_tab`, waiting for a validated destination.
#[cfg(target_arch = "wasm32")]
pub(in super::super) struct PendingTab {
    window: web_sys::Window,
    kept: std::cell::Cell<bool>,
}

/// A reserved tab that was never navigated must not be left blank forever.
#[cfg(target_arch = "wasm32")]
impl Drop for PendingTab {
    fn drop(&mut self) {
        if !self.kept.get() {
            let _ = self.window.close();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(in super::super) struct PendingTab;

impl PendingTab {
    /// Navigate the reserved tab; an error leaves it open for the caller to
    /// close.
    pub(in super::super) fn navigate(&self, href: &str) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            let navigated = self.window.location().set_href(href);
            if navigated.is_ok() {
                self.kept.set(true);
            }
            navigated.map_err(|_| "The browser refused the address.".to_owned())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = href;
            Err("Links open only in the browser.".to_owned())
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(super) fn document(&self) -> Option<web_sys::Document> {
        if self.window.closed().unwrap_or(true) {
            None
        } else {
            self.window.document()
        }
    }

    /// Close a tab that will never receive its destination.
    pub(super) fn close(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.kept.set(true);
            let _ = self.window.close();
        }
    }
}

/// Reserve an about:blank tab and sever its opener.
///
/// `noopener` is deliberately NOT passed to `open`: with it the call returns
/// `null` even on success, which would misreport a working popup as blocked.
#[cfg(target_arch = "wasm32")]
pub(in super::super) fn open_blank_tab() -> Result<PendingTab, String> {
    let window = web_sys::window().ok_or("This document has no window.")?;
    let opened = window
        .open_with_url_and_target("about:blank", "_blank")
        .map_err(|_| "The browser blocked the new tab.".to_owned())?
        .ok_or("The browser blocked the new tab. Allow pop-ups for this site.")?;
    if !js_sys::Reflect::set(&opened, &"opener".into(), &wasm_bindgen::JsValue::NULL)
        .unwrap_or(false)
    {
        let _ = opened.close();
        return Err("The browser could not isolate the new tab safely.".to_owned());
    }
    Ok(PendingTab {
        window: opened,
        kept: std::cell::Cell::new(false),
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(in super::super) fn open_blank_tab() -> Result<PendingTab, String> {
    Err("Links open only in the browser.".to_owned())
}

/// Resolve a validated same-origin capability path against this document.
#[cfg(target_arch = "wasm32")]
pub(super) fn absolute_document_url(path: &str) -> Result<String, String> {
    let window = web_sys::window().ok_or("This document has no window.")?;
    let base = window
        .location()
        .href()
        .map_err(|_| "This document has no address.".to_owned())?;
    let url = web_sys::Url::new_with_base(path, &base)
        .map_err(|_| "The stats dashboard address is invalid.".to_owned())?;
    Ok(url.href())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn absolute_document_url(_path: &str) -> Result<String, String> {
    Err("The stats dashboard opens only in the browser.".to_owned())
}

/// The browser's transient activation, when it exposes the answer.
#[cfg(target_arch = "wasm32")]
pub(super) fn user_activation_active() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::get(&window.navigator(), &"userActivation".into())
        .ok()
        .and_then(|activation| {
            js_sys::Reflect::get(&activation, &"isActive".into())
                .ok()
                .and_then(|active| active.as_bool())
        })
        .unwrap_or(false)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn user_activation_active() -> bool {
    false
}
