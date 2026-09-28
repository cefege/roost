//! One shaped Connect call: exactly what a transport is handed, and nothing it
//! has to remember to add.
//!
//! The fields are private and the only constructor is crate-internal, reached
//! through `ConnectClient`. That is what makes "every request carries the tab
//! id" structural rather than a rule somebody can forget on the one code path
//! that builds a request by hand — a `Option<String>` tab id would be a second
//! spelling of the same fence, and a request with no tab id is refused by the
//! coordinator with a different error than one that sends it empty
//! (`protocol/spec/auth-and-pairing.md`, tab fence).

/// A unary call, shaped and ready for a transport to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    method: &'static str,
    body: Vec<u8>,
    bearer: Option<String>,
    tab_id: String,
    call_id: u64,
}

impl ConnectRequest {
    /// Shape one call. Crate-internal because the tab id is not optional and
    /// only `ConnectClient` knows which tab this client presents.
    pub(crate) fn new(
        method: &'static str,
        body: Vec<u8>,
        bearer: Option<String>,
        tab_id: String,
        call_id: u64,
    ) -> Self {
        Self {
            method,
            body,
            bearer,
            tab_id,
            call_id,
        }
    }

    /// The call this request answers, so a host can correlate the answer with
    /// the `Effect::Rpc` that asked for it.
    pub fn call_id(&self) -> u64 {
        self.call_id
    }

    /// The Connect method name, e.g. `SessionsList`.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// The encoded request message, as `client::rpc::codec::encode_rpc_request`
    /// produced it. The core never encodes; the host calls the codec.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The bearer, when one could be minted.
    ///
    /// `None` is a real state and the request still goes out — that is the whole
    /// rule, and it is why this is an accessor and not a required field.
    pub fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
    }

    /// The tab this call is made on behalf of. Never absent, and not derived
    /// from the body: three SPA tabs share one device key, and the header is the
    /// only thing that tells them apart.
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    /// Whether this call goes out with no credential.
    pub fn is_unauthenticated(&self) -> bool {
        self.bearer.is_none()
    }
}
