//! A fresh terminal view id, minted by the host with the browser's own entropy.
//!
//! One function for both callers — the pane that opens a view and the pump that
//! answers a `MintTerminalViewId` effect — because there is one rule and it is a
//! security-shaped one: the id is `crypto.randomUUID()`, and `crypto` is only
//! reachable from a secure context. A non-secure origin throws rather than
//! returns, so the answer there is "no id", never a fabricated one.
//!
//! `roost_client_core` names no DOM type and takes no RNG, which is why this lives
//! here rather than in the core: the core can only ASK, through
//! `Effect::MintTerminalViewId`, and the host answers with
//! `ClientEvent::TerminalViewIdMinted`. A core that minted ids itself would be a
//! second answer to "where does a view id come from", from a crate that cannot
//! reach a CSPRNG.

/// A fresh view id, or `None` when this document cannot mint one.
///
/// `None` is a real answer. The worker refuses any view id that is not a UUID
/// (v2 `validateTerminalViewCommand`: "invalid terminal view id"), so a fallback
/// of some other shape would be refused on the wire and there is nothing to gain
/// by inventing one here.
#[cfg(target_arch = "wasm32")]
pub fn mint_view_id() -> Option<String> {
    web_sys::window()
        .filter(web_sys::Window::is_secure_context)
        .and_then(|window| window.crypto().ok())
        .map(|crypto| crypto.random_uuid())
}

/// A build with no browser has no `crypto`, and mints nothing.
#[cfg(not(target_arch = "wasm32"))]
pub fn mint_view_id() -> Option<String> {
    None
}
