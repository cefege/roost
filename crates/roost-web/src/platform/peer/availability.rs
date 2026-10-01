//! Whether this document can do WebRTC at all, and the pin a native test uses
//! to answer for it.
//!
//! Owned by `platform::peer`. Split from the adapter because the question is
//! asked BEFORE a peer exists — `Pump::new` declares it to the carrier lane so a
//! document that cannot peer never makes a worker install a peer credential for
//! it — and because the pin is the one half of the answer a test binary can
//! reach: a native build has no `RTCPeerConnection`, so the production probe is
//! unexercisable off-web while the DECLARATION the probe feeds is pure.
//!
//! Host-only, like the branch of `is_available` the pin feeds: this crate ships
//! to `wasm32`, so nothing that reaches a browser can reach the pin.

/// Whether this build has a browser WebRTC stack at all.
///
/// False everywhere except a `wasm32` build, and false there too unless the
/// document exposes the constructor.
pub fn is_available() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Reflect::get(
            &js_sys::global(),
            &wasm_bindgen::JsValue::from_str(super::dom::RTC_PEER_CONNECTION),
        )
        .map(|value| value.is_function())
        .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        forced_availability().unwrap_or(false)
    }
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static FORCED_AVAILABILITY: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The answer `is_available` gives when a test has pinned it, if any.
#[cfg(not(target_arch = "wasm32"))]
fn forced_availability() -> Option<bool> {
    FORCED_AVAILABILITY.with(|forced| forced.get())
}

/// Pin what `is_available` reports.
///
/// What this guards is not the platform having a stack — that is the production
/// probe — but whether the host DECLARES its capability to the lane. `Pump::new`
/// declares before any view can demand a session, and a lane whose host never
/// spoke parks every machine as `Unsupported` and keeps the session on Sync for
/// the life of the document.
#[cfg(not(target_arch = "wasm32"))]
pub fn force_availability_for_test(available: bool) {
    FORCED_AVAILABILITY.with(|forced| forced.set(Some(available)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_capability_is_what_is_available_reports() {
        // The pin is per thread and per process, and every assertion about the
        // lane's four gates depends on it, so the reset afterwards is part of
        // the test rather than tidiness.
        force_availability_for_test(true);
        assert!(is_available(), "a document that declared a stack can peer");
        force_availability_for_test(false);
        assert!(!is_available(), "a document that declared no stack parks");
        force_availability_for_test(false);
    }
}
