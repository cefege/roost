//! The client: shape one call, hand it to the host, never drop it.
//!
//! This is the piece `Effect::Rpc` names but cannot be. The core asks for a
//! call; a host performs it; this is what the two of them agree the call LOOKS
//! like on the way out, and it is where v2's interceptor rules live.
//!
//! It is a struct over one string — the tab id — because that is the only thing
//! a Connect request carries that the core does not already know. It holds no
//! state machine, performs no I/O, and never retries: every call here is either
//! idempotent (so the host's dial loop decides) or a ceremony step a human
//! drives.

use crate::client::rpc::credential::Credential;
use crate::client::rpc::methods::{connect_call_id, connect_method};
use crate::client::rpc::request::ConnectRequest;
use crate::effect::RpcCall;

/// What a host does with a shaped call.
///
/// Synchronous on purpose, and returning nothing: the transport moves the bytes
/// and answers later as `ClientEvent::RpcResultReceived`. A method returning a
/// future would put a runtime type in this crate's public API, which is the one
/// thing `docs/phase4-client-contract.md` §2 exists to prevent. The core itself
/// never calls this — it emits `Effect::Rpc` and waits — so a host implements it
/// once and every front end gets the interceptor's two rules for free.
pub trait ConnectDispatcher {
    /// Perform one call. The answer arrives as `ClientEvent::RpcResultReceived`.
    fn dispatch(&self, request: ConnectRequest);
}

/// The client, over the tab every call is made on behalf of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectClient {
    tab_id: String,
}

impl ConnectClient {
    /// A client presenting `tab_id`.
    ///
    /// Taken rather than defaulted because a call with no tab is refused by the
    /// coordinator's fence with a different error than one that sends it empty,
    /// so an invented id would hide a real misconfiguration behind a
    /// working-looking client — the same reason `ClientCore::new` requires one.
    pub fn new(tab_id: impl Into<String>) -> Self {
        Self {
            tab_id: tab_id.into(),
        }
    }

    /// The tab this client presents.
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    /// Shape one call without dispatching it.
    ///
    /// A host that must mint the credential between shaping and sending shapes
    /// with an unavailable credential, mints, and re-shapes — which is why
    /// `prepare` is public and takes the credential as an argument rather than
    /// reading one from somewhere.
    pub fn prepare(&self, call: &RpcCall, body: Vec<u8>, credential: Credential) -> ConnectRequest {
        if let Some(reason) = credential.reason() {
            tracing::warn!(
                target: "rpc",
                method = %connect_method(call),
                call_id = connect_call_id(call),
                reason = %reason,
                "device credential unavailable; dispatching the request unauthenticated"
            );
        }
        ConnectRequest::new(
            connect_method(call),
            body,
            credential.bearer().map(str::to_owned),
            self.tab_id.clone(),
            connect_call_id(call),
        )
    }

    /// Shape one call and hand it to the host. Returns what went out.
    pub fn dispatch(
        &self,
        dispatcher: &dyn ConnectDispatcher,
        call: &RpcCall,
        body: Vec<u8>,
        credential: Credential,
    ) -> ConnectRequest {
        let request = self.prepare(call, body, credential);
        dispatcher.dispatch(request.clone());
        request
    }
}
