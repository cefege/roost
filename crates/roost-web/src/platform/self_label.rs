//! A human-readable label for THIS browser ("Chrome — macOS"), sent to the
//! coordinator when it pairs, stored as the key's label and shown on viewer
//! chips instead of a fingerprint prefix.
//!
//! Called by the pump's pair redeem and the pairing ceremony. Ported from
//! `apps/web/src/browser/browserSelfLabel.ts`: `userAgentData` first, the UA
//! string as the fallback, because a browser cannot read the OS hostname.

/// The label from what the browser reports.
pub fn browser_self_label(user_agent: &str, ua_platform: Option<&str>, brands: &[String]) -> String {
    let platform = ua_platform
        .filter(|platform| !platform.is_empty())
        .map(str::to_owned)
        .or_else(|| platform_from_ua(user_agent).map(str::to_owned));
    let browser = browser_from_brands(brands).or_else(|| browser_from_ua(user_agent).map(str::to_owned));
    match (browser, platform) {
        (Some(browser), Some(platform)) => format!("{browser} — {platform}"),
        (Some(browser), None) => browser,
        (None, Some(platform)) => platform,
        (None, None) => "browser".to_owned(),
    }
}

fn browser_from_brands(brands: &[String]) -> Option<String> {
    let real = brands.iter().find(|brand| {
        let lower = brand.to_ascii_lowercase();
        !(lower.starts_with("not") && lower.contains("brand")) && !lower.contains("chromium")
    });
    real.or_else(|| brands.first()).cloned()
}

fn browser_from_ua(user_agent: &str) -> Option<&'static str> {
    if user_agent.contains("Firefox/") {
        Some("Firefox")
    } else if user_agent.contains("Edg/") {
        Some("Edge")
    } else if user_agent.contains("Chrome/") {
        Some("Chrome")
    } else if user_agent.contains("Safari/") {
        Some("Safari")
    } else {
        None
    }
}

fn platform_from_ua(user_agent: &str) -> Option<&'static str> {
    if user_agent.contains("Mac OS X") || user_agent.contains("Macintosh") {
        Some("macOS")
    } else if user_agent.contains("Windows") {
        Some("Windows")
    } else if user_agent.contains("Linux") {
        Some("Linux")
    } else if user_agent.contains("iPhone") || user_agent.contains("iPad") {
        Some("iOS")
    } else if user_agent.contains("Android") {
        Some("Android")
    } else {
        None
    }
}

/// This browser's label, read from `navigator`.
#[cfg(target_arch = "wasm32")]
pub fn current_browser_self_label() -> String {
    use wasm_bindgen::JsValue;
    let Some(navigator) = web_sys::window().map(|window| window.navigator()) else {
        return browser_self_label("", None, &[]);
    };
    let user_agent = navigator.user_agent().unwrap_or_default();
    let data = js_sys::Reflect::get(&navigator, &JsValue::from_str("userAgentData")).ok();
    let platform = data
        .as_ref()
        .and_then(|data| js_sys::Reflect::get(data, &JsValue::from_str("platform")).ok())
        .and_then(|value| value.as_string());
    let brands: Vec<String> = data
        .as_ref()
        .and_then(|data| js_sys::Reflect::get(data, &JsValue::from_str("brands")).ok())
        .map(|brands| js_sys::Array::from(&brands))
        .map(|brands| {
            brands
                .iter()
                .filter_map(|entry| {
                    js_sys::Reflect::get(&entry, &JsValue::from_str("brand"))
                        .ok()
                        .and_then(|brand| brand.as_string())
                })
                .collect()
        })
        .unwrap_or_default();
    browser_self_label(&user_agent, platform.as_deref(), &brands)
}

/// A build with no browser reports the generic label.
#[cfg(not(target_arch = "wasm32"))]
pub fn current_browser_self_label() -> String {
    browser_self_label("", None, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_data_wins_and_the_grease_brand_is_skipped() {
        let brands = vec!["Not.A/Brand".to_owned(), "Chromium".to_owned(), "Google Chrome".to_owned()];
        assert_eq!(browser_self_label("", Some("macOS"), &brands), "Google Chrome — macOS");
    }

    #[test]
    fn the_ua_string_is_the_fallback_and_edge_is_not_chrome() {
        let edge = "Mozilla/5.0 (Windows NT 10.0) AppleWebKit Chrome/120 Safari/537 Edg/120";
        assert_eq!(browser_self_label(edge, None, &[]), "Edge — Windows");
        let firefox = "Mozilla/5.0 (X11; Linux x86_64) Gecko Firefox/128";
        assert_eq!(browser_self_label(firefox, None, &[]), "Firefox — Linux");
        assert_eq!(browser_self_label("curl/8", None, &[]), "browser");
    }
}
