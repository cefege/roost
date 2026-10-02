//! Fresh terminal identities — view ids and peer ids — minted by the host with
//! the browser's own entropy.
//!
//! One rule for every caller — the pane that opens a view, the pump that answers
//! a `MintTerminalViewId` effect, and the peer lane that opens a transport —
//! because it is a security-shaped one: the id is `crypto.randomUUID()`, and
//! `crypto` is only reachable from a secure context. A non-secure origin throws
//! rather than returns, so the answer there is "no id", never a fabricated one.
//!
//! `roost_client_core` names no DOM type and takes no RNG, which is why this lives
//! here rather than in the core: the core can only ASK, through
//! `Effect::MintTerminalViewId` or `CarrierEffect::OpenTransport`, and the host
//! answers. A core that minted ids itself would be a second answer to "where does
//! an id come from", from a crate that cannot reach a CSPRNG.

/// A fresh view id, or `None` when this document cannot mint one.
///
/// `None` is a real answer. The worker refuses any view id that is not a UUID
/// (v2 `validateTerminalViewCommand`: "invalid terminal view id"), so a fallback
/// of some other shape would be refused on the wire and there is nothing to gain
/// by inventing one here.
pub fn mint_view_id() -> Option<String> {
    random_uuid()
}

/// A fresh peer id for one WebRTC negotiation, or `None` when this document
/// cannot mint one. The coordinator refuses a peer id that is not a UUID
/// ("terminal peer peer_id is invalid"); v2 mints it the same way
/// (`createTerminalDirectRequestId`).
pub fn mint_peer_id() -> Option<String> {
    random_uuid()
}

#[cfg(target_arch = "wasm32")]
fn random_uuid() -> Option<String> {
    web_sys::window()
        .filter(web_sys::Window::is_secure_context)
        .and_then(|window| window.crypto().ok())
        .map(|crypto| crypto.random_uuid())
}

/// A build with no browser has no `crypto`, and mints nothing.
#[cfg(not(target_arch = "wasm32"))]
fn random_uuid() -> Option<String> {
    None
}
