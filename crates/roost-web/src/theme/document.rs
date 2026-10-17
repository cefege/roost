//! The browser half of the theme engine: reading the OS colour-scheme
//! preference and writing a `ThemeApplication` onto `document.documentElement`.
//! Called only by `theme.rs`, which follows OS flips through `use_media_query`;
//! every rule it performs is decided in `choice.rs`.

use wasm_bindgen::JsCast as _;

use super::ThemeDocument;
use super::choice::{ThemeApplication, managed_properties};
use super::tokens::ThemeAppearance;

/// The media query whose match means "the OS prefers dark".
pub(super) const PREFERS_DARK_QUERY: &str = "(prefers-color-scheme: dark)";

/// rust-ui's `dark:` variant selects on this class (`&:is(.dark *)`).
const DARK_CLASS: &str = "dark";

/// `document.documentElement` and the OS preference behind it.
#[derive(Debug)]
pub struct BrowserThemeDocument;

impl ThemeDocument for BrowserThemeDocument {
    fn system_appearance(&self) -> Option<ThemeAppearance> {
        let prefers_dark =
            crate::components::hooks::use_media_query::media_query_match(PREFERS_DARK_QUERY)?;
        Some(if prefers_dark {
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
        for property in managed_properties() {
            let _ = style.remove_property(property);
        }
        for (property, value) in &application.properties {
            let _ = style.set_property(property, value);
        }
        let scheme = application.appearance.color_scheme();
        let classes = root.class_list();
        let _ = match application.appearance {
            ThemeAppearance::Dark => classes.add_1(DARK_CLASS),
            ThemeAppearance::Light => classes.remove_1(DARK_CLASS),
        };
        // Tailwind's `dark:` variant also matches on this attribute (tailwind.css).
        let _ = root.set_attribute("data-color-scheme", scheme);
        let _ = style.set_property("color-scheme", scheme);
    }
}
