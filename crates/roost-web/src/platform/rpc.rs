//! Connect unary over `fetch`.
//!
//! Owned by `platform`, called by the Connect client slice, and depends on
//! `web-sys`'s `Request`/`Response` and nothing from the client core: the
//! transport moves bytes and reports a refusal verbatim, and decoding either end
//! belongs to whoever owns the generated types.
//!
//! The shape it reproduces is v2's `createConnectTransport({ useBinaryFormat:
//! true })` (`apps/web/src/client/rpc/connect.ts`): one POST per call, a
//! protobuf body with no five-byte envelope, and three headers that are not
//! optional. It is UNARY because every RPC in the contract is unary; adding a
//! server-streaming method to this trait would be a protocol change, not a
//! transport change.

use std::fmt;

use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

/// The content type a binary Connect unary call uses.
pub const CONNECT_BINARY_CONTENT_TYPE: &str = "application/proto";

/// The Connect protocol version this transport speaks.
pub const CONNECT_PROTOCOL_VERSION: &str = "1";

/// The header the coordinator stamps with the auth layer that decided a request.
pub const AUTH_LAYER_HEADER: &str = "x-roost-auth-layer";

/// The header naming the tab every call is made on behalf of.
pub const TAB_ID_HEADER: &str = "x-roost-tab-id";

/// One unary call, addressed by its last path segment.
///
/// The segment, not the whole path: the service prefix is `roost.v1`'s to
/// publish and this transport's to prepend, and a caller that spells out
/// `/roost.v1.CoordinatorService/SessionsList` by hand is a second place the
/// service name lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryRequest {
    /// The method name, e.g. `SessionsList`.
    pub method: &'static str,
    /// The encoded request message.
    pub body: Vec<u8>,
    /// The bearer credential, when one could be minted.
    ///
    /// `None` is a real state and the caller sends anyway: v2's interceptor
    /// caught a signing failure, signalled it, and issued the request
    /// unauthenticated, because a device that cannot sign is a device whose
    /// bootstrap calls still have to reach the coordinator.
    pub bearer: Option<String>,
}

/// What the coordinator answered, refusal included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryResponse {
    /// The HTTP status.
    pub status: u16,
    /// The `content-type` the answer arrived with.
    pub content_type: String,
    /// The undecoded body. An error body is a framed `google.rpc.Status`, and
    /// decoding it needs the generated types, so it travels whole.
    pub body: Vec<u8>,
    /// The `x-roost-auth-layer` header, when the coordinator sent one.
    pub auth_layer: Option<String>,
}

/// Why a call cannot be treated as an answer.
///
/// Two cases, and the difference between them is the whole error model: a
/// `Network` failure is a transport problem the caller may retry, while a
/// `Refused` is the coordinator's considered answer and must not be retried into
/// the same rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTransportError {
    /// The request never produced an HTTP response.
    Network {
        /// What the browser reported.
        message: String,
    },
    /// The coordinator refused the call. The response is whole so the caller
    /// can read the Connect code out of the body and the auth layer out of the
    /// headers — v2's `classifyAuthFailure` needs both.
    Refused(UnaryResponse),
    /// The response body could not be read. A distinct case from `Network`
    /// because the request DID reach the coordinator and may have committed.
    UnreadableBody {
        /// What the browser reported.
        message: String,
    },
}

impl fmt::Display for ConnectTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network { message } => write!(
                formatter,
                "the request never reached the coordinator: {message}"
            ),
            Self::Refused(response) => write!(
                formatter,
                "the coordinator refused the call with status {}",
                response.status
            ),
            Self::UnreadableBody { message } => {
                write!(formatter, "the response body could not be read: {message}")
            }
        }
    }
}

impl std::error::Error for ConnectTransportError {}

/// A Connect unary transport, as the client core's callers see it.
pub trait ConnectTransport {
    /// The origin every call is made against.
    fn base_url(&self) -> &str;

    /// Issue one unary call.
    fn call_unary(
        &self,
        request: UnaryRequest,
    ) -> impl Future<Output = Result<UnaryResponse, ConnectTransportError>>;
}

/// The browser's `fetch`, wrapped as a Connect unary transport.
#[derive(Debug, Clone)]
pub struct FetchConnectTransport {
    base_url: String,
    tab_id: String,
}

impl FetchConnectTransport {
    /// A transport against `base_url`, presenting `tab_id` on every call.
    ///
    /// The tab id is taken once rather than per call because it is the identity
    /// of THIS document, and v2 set it unconditionally and independently of auth:
    /// a request that arrives without it cannot be correlated with a socket, and
    /// the coordinator uses it to attribute the row in `audit_log`.
    pub fn new(base_url: impl Into<String>, tab_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            tab_id: tab_id.into(),
        }
    }

    /// The full URL one method is called at.
    pub fn method_url(&self, method: &str) -> String {
        format!(
            "{}/roost.v1.CoordinatorService/{method}",
            self.base_url.trim_end_matches('/')
        )
    }

    fn request_init(&self, request: &UnaryRequest) -> Result<RequestInit, ConnectTransportError> {
        let headers = Headers::new().map_err(js_error)?;
        headers
            .append("content-type", CONNECT_BINARY_CONTENT_TYPE)
            .map_err(js_error)?;
        headers
            .append("connect-protocol-version", CONNECT_PROTOCOL_VERSION)
            .map_err(js_error)?;
        headers
            .append(TAB_ID_HEADER, &self.tab_id)
            .map_err(js_error)?;
        if let Some(bearer) = &request.bearer {
            headers
                .append("authorization", &format!("Bearer {bearer}"))
                .map_err(js_error)?;
        }
        let body = js_sys::Uint8Array::from(request.body.as_slice());
        let init = RequestInit::new();
        init.set_method("POST");
        init.set_headers(&headers);
        init.set_body(body.as_ref());
        Ok(init)
    }
}

impl ConnectTransport for FetchConnectTransport {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn call_unary(
        &self,
        request: UnaryRequest,
    ) -> Result<UnaryResponse, ConnectTransportError> {
        let url = self.method_url(request.method);
        let init = self.request_init(&request)?;
        let http_request = Request::new_with_str_and_init(&url, &init).map_err(|error| {
            ConnectTransportError::Network {
                message: describe(&error),
            }
        })?;
        let window = web_sys::window().ok_or_else(|| ConnectTransportError::Network {
            message: "no window: this build is not running in a document".to_string(),
        })?;
        let promise = window.fetch_with_request(&http_request);
        let response_value =
            JsFuture::from(promise)
                .await
                .map_err(|error| ConnectTransportError::Network {
                    message: describe(&error),
                })?;
        let response: Response =
            response_value
                .dyn_into()
                .map_err(|_| ConnectTransportError::UnreadableBody {
                    message: "fetch resolved with something that is not a Response".to_string(),
                })?;

        let status = response.status();
        let content_type = header_value(&response, "content-type").unwrap_or_default();
        let auth_layer = header_value(&response, AUTH_LAYER_HEADER);
        let buffer = JsFuture::from(response.array_buffer()?)
            .await
            .map_err(|error| ConnectTransportError::UnreadableBody {
                message: describe(&error),
            })?;
        let body = js_sys::Uint8Array::new(&buffer).to_vec();

        let answered = UnaryResponse {
            status,
            content_type,
            body,
            auth_layer,
        };
        if (200..300).contains(&status) {
            Ok(answered)
        } else {
            Err(ConnectTransportError::Refused(answered))
        }
    }
}

impl From<wasm_bindgen::JsValue> for ConnectTransportError {
    fn from(value: wasm_bindgen::JsValue) -> Self {
        Self::Network {
            message: describe(&value),
        }
    }
}

/// One header off a response, or `None` when it is absent or unreadable.
fn header_value(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name).ok().flatten()
}

/// A rejected `JsValue` as the string the browser gave for it.
fn describe(value: &wasm_bindgen::JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

fn js_error(value: wasm_bindgen::JsValue) -> ConnectTransportError {
    ConnectTransportError::Network {
        message: describe(&value),
    }
}
