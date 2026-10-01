//! Whether the BROWSER thinks it has a route to the network.
//!
//! One read, two callers: the connection banner decides whether to raise, and
//! the status bar decides whether to name the coordinator. Two copies of
//! `navigator.onLine` behind a `cfg` pair is two answers to one question, and
//! the two surfaces that must agree about an outage are exactly the two that
//! would drift.
//!
//! A missing `navigator` — a native build, a prerender — reads as ONLINE,
//! because the thing both callers are reporting on is the client, not the
//! browser's guess about it.

/// `navigator.onLine`, or `true` where there is no browser to ask.
#[must_use]
pub fn browser_online() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window().is_none_or(|window| window.navigator().on_line())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        true
    }
}
