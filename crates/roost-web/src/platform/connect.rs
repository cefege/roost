//! The coordinator Connect client the app uses: the device key's bearer on
//! every call, the tab id header, and the answer decoded by the client core.
//!
//! Owned by the pump (one per document, in context); called by the pump for the
//! core's `Effect::Rpc` and by surfaces for `client::rpc::calls::*`. Depends on
//! `platform::rpc` (the fetch transport) and `platform::device_key`. Ported from
//! `apps/web/src/client/rpc/connect.ts` (`coordClient`, `publicCoordClient`,
//! the bearer interceptor at `:117-132`).

use std::cell::RefCell;
use std::rc::Rc;

use roost_client_core::client::rpc::{
    CallError, ConnectError, UnaryMethod, connect_method, decode_rpc_response, encode_rpc_request,
};
use roost_client_core::{RpcCall, RpcResult};

use crate::platform::clock::WallClock;
use crate::platform::device_key::WebDeviceKey;
use crate::platform::rpc::{
    ConnectTransport, ConnectTransportError, FetchConnectTransport, UnaryRequest,
};

/// The app's coordinator client.
#[derive(Debug)]
pub struct CoordRpc {
    transport: FetchConnectTransport,
    key: RefCell<Option<Rc<WebDeviceKey>>>,
    clock: WallClock,
}

impl CoordRpc {
    /// A client against `base_url`, presenting `tab_id` on every call.
    pub fn new(base_url: impl Into<String>, tab_id: impl Into<String>) -> Self {
        Self {
            transport: FetchConnectTransport::new(base_url, tab_id),
            key: RefCell::new(None),
            clock: WallClock,
        }
    }

    /// The origin every call goes to.
    pub fn base_url(&self) -> &str {
        self.transport.base_url()
    }

    /// Install the loaded device key; calls before this go out unauthenticated.
    pub fn install_key(&self, key: Rc<WebDeviceKey>) {
        tracing::info!(target: "rpc", fingerprint = key.fingerprint(), "device key installed");
        *self.key.borrow_mut() = Some(key);
    }

    /// The device key, once loaded.
    pub fn device_key(&self) -> Option<Rc<WebDeviceKey>> {
        self.key.borrow().clone()
    }

    /// A bearer for a call made now, or `None` when there is no key or it
    /// cannot sign. v2's interceptor sends the request anyway: a device that
    /// cannot sign must still reach the pairing gate.
    pub async fn bearer(&self) -> Option<String> {
        let key = self.device_key()?;
        match key.bearer(self.clock.now_ms()).await {
            Ok(bearer) => Some(bearer),
            Err(reason) => {
                tracing::warn!(
                    target: "rpc",
                    %reason,
                    "device credential unavailable; dispatching the request unauthenticated"
                );
                None
            }
        }
    }

    /// Call one method with the device credential.
    pub async fn call<M: UnaryMethod>(&self, request: &M) -> Result<M::Response, CallError> {
        let body = request.encode_request().map_err(CallError::Codec)?;
        let bearer = self.bearer().await;
        let answer = self.send(M::METHOD, body, bearer).await?;
        M::decode_response(&answer).map_err(CallError::Codec)
    }

    /// Call one method WITHOUT a credential (v2 `publicCoordClient`).
    pub async fn call_public<M: UnaryMethod>(&self, request: &M) -> Result<M::Response, CallError> {
        let body = request.encode_request().map_err(CallError::Codec)?;
        let answer = self.send(M::METHOD, body, None).await?;
        M::decode_response(&answer).map_err(CallError::Codec)
    }

    /// Perform one of the core's calls and map the answer back for
    /// `ClientEvent::RpcResultReceived`. Identity discovery is the one public
    /// call (`sync-bootstrap.ts:177`); every other call carries the bearer.
    pub async fn call_core(&self, call: &RpcCall) -> RpcResult {
        let call_id = roost_client_core::client::rpc::methods::connect_call_id(call);
        let method = connect_method(call);
        let failed = |error: CallError| RpcResult::Failed { call_id, error };
        let body = match encode_rpc_request(call) {
            Ok(body) => body,
            Err(error) => return failed(CallError::Codec(error)),
        };
        let bearer = match call {
            RpcCall::CoordIdentity { .. } => None,
            _ => self.bearer().await,
        };
        match self.send(method, body, bearer).await {
            Ok(answer) => decode_rpc_response(call, &answer)
                .unwrap_or_else(|error| failed(CallError::Codec(error))),
            Err(error) => failed(error),
        }
    }

    async fn send(
        &self,
        method: &'static str,
        body: Vec<u8>,
        bearer: Option<String>,
    ) -> Result<Vec<u8>, CallError> {
        let request = UnaryRequest {
            method,
            body,
            bearer,
        };
        match self.transport.call_unary(request).await {
            Ok(response) => Ok(response.body),
            Err(ConnectTransportError::Refused(response)) => {
                let error =
                    ConnectError::from_answer(response.status, &response.body, response.auth_layer);
                tracing::warn!(
                    target: "rpc",
                    method,
                    code = ?error.code,
                    message = %error.message,
                    "coordinator refused the call"
                );
                Err(CallError::Connect(error))
            }
            Err(ConnectTransportError::Network { message })
            | Err(ConnectTransportError::UnreadableBody { message }) => {
                tracing::warn!(target: "rpc", method, %message, "call did not complete");
                Err(CallError::Network(message))
            }
        }
    }
}
