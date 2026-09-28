//! The browser half of the theme engine: reading the OS colour-scheme
//! preference, writing a `ThemeApplication` onto `document.documentElement`, and
//! listening for the OS preference to flip. Called only by `theme.rs`; every
//! rule it performs is decided in `choice.rs`. Ports the DOM calls of
//! `apps/web/src/lib/theme.ts` (`systemThemeId`, `applyTheme`'s writes, and the
//! module-level `matchMedia` listener).

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::ThemeDocument;
use super::choice::ThemeApplication;
use super::tokens::ThemeAppearance;

/// The media query whose match means "the OS prefers dark".
const PREFERS_DARK_QUERY: &str = "(prefers-color-scheme: dark)";

/// `document.documentElement` and the OS preference behind it.
#[derive(Debug)]
pub struct BrowserThemeDocument;

impl ThemeDocument for BrowserThemeDocument {
    fn system_appearance(&self) -> Option<ThemeAppearance> {
        let query = web_sys::window()?.match_media(PREFERS_DARK_QUERY).ok()??;
        Some(if query.matches() {
            ThemeAppearance::Dark
        } else {
            ThemeAppearance::Light
        })
    }

    fn write(&self, application: &ThemeApplication) {
        let Some(root) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            tracing::warn!(target: "theme", "no document element to theme");
            return;
        };
        let style = root.style();
        for (property, value) in &application.properties {
            let _ = style.set_property(property, value);
        }
        let _ = root.set_attribute("data-theme", application.theme_id);
        let _ = style.set_property("color-scheme", application.color_scheme);
    }
}

/// Call `on_flip` whenever the OS light/dark preference changes.
///
/// `Closure::forget` is deliberate: the listener lives as long as the document,
/// and it is installed once, at boot.
pub fn follow_system_appearance(on_flip: impl Fn() + 'static) {
    let Some(query) = web_sys::window()
        .and_then(|window| window.match_media(PREFERS_DARK_QUERY).ok())
        .flatten()
    else {
        tracing::debug!(target: "theme", "matchMedia unavailable; the theme will not follow OS flips");
        return;
    };
    let listener = Closure::<dyn Fn(web_sys::MediaQueryListEvent)>::new(move |_event| on_flip());
    if query
        .add_event_listener_with_callback("change", listener.as_ref().unchecked_ref())
        .is_err()
    {
        tracing::warn!(target: "theme", "the browser refused the colour-scheme listener");
    }
    listener.forget();
}
