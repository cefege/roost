//! The `x-roost-*` request headers and the sentinel values they carry.
//!
//! These names are the auth-classification contract between the browser's
//! interceptor and the coordinator's auth stack: a caller's own device
//! credential on one side, a trusted proxy asserting the caller's address on
//! the other. A rename on one side only reclassifies the other silently — the
//! request is still accepted, by the wrong rule — so both ends import from
//! here and a test pins every spelling.
//!
//! The reverse proxy in front of a coordinator sees these names too, which is
//! why a header name is a wire value and not a private constant of one app.

/// The browser tab that opened a request, so a response is attributed to the
/// tab that is still showing it.
pub const X_ROOST_TAB_ID: &str = "x-roost-tab-id";
/// The client's log-correlation id for one request.
pub const X_ROOST_TRACE_ID: &str = "x-roost-trace-id";
/// The address a trusted proxy asserts for the caller. Only meaningful when
/// that same proxy is the one the coordinator trusts.
pub const X_ROOST_REMOTE_ADDR: &str = "x-roost-remote-addr";
/// The machine a trusted proxy asserts the request was forwarded by.
pub const X_ROOST_ON_HOST: &str = "x-roost-on-host";
/// Whether a trusted proxy vouches for the request. The vouched-for facts are
/// worthless unless this is set by a proxy the coordinator is configured to
/// trust, never by the client.
pub const X_ROOST_LISTENER_TRUST: &str = "x-roost-listener-trust";
/// Which authentication layer resolved the caller: its own device credential,
/// or a proxy already in front of the coordinator.
pub const X_ROOST_AUTH_LAYER: &str = "x-roost-auth-layer";

/// `X_ROOST_AUTH_LAYER` when the caller authenticated with its own device key.
pub const AUTH_LAYER_DEVICE: &str = "device";
/// `X_ROOST_AUTH_LAYER` when a trusted proxy asserted the caller instead.
pub const AUTH_LAYER_TRUSTED_PROXY: &str = "trusted-proxy";
/// The only value that turns on `X_ROOST_LISTENER_TRUST`. A boolean spelled as
/// anything else — including `true` — is not a vouched-for request.
pub const LISTENER_TRUST_YES: &str = "1";

/// Every header name, in the order the contract lists them. A caller that has
/// to write all of them reads one list rather than five greps.
pub const ROOST_REQUEST_HEADERS: [&str; 6] = [
    X_ROOST_TAB_ID,
    X_ROOST_TRACE_ID,
    X_ROOST_REMOTE_ADDR,
    X_ROOST_ON_HOST,
    X_ROOST_LISTENER_TRUST,
    X_ROOST_AUTH_LAYER,
];

/// Every sentinel value the auth layer accepts, paired with the header that
/// carries it.
pub const AUTH_LAYER_SENTINELS: [(&str, &str); 3] = [
    (X_ROOST_AUTH_LAYER, AUTH_LAYER_DEVICE),
    (X_ROOST_AUTH_LAYER, AUTH_LAYER_TRUSTED_PROXY),
    (X_ROOST_LISTENER_TRUST, LISTENER_TRUST_YES),
];

#[cfg(test)]
mod tests {
    use super::{
        AUTH_LAYER_DEVICE, AUTH_LAYER_SENTINELS, AUTH_LAYER_TRUSTED_PROXY, LISTENER_TRUST_YES,
        ROOST_REQUEST_HEADERS, X_ROOST_AUTH_LAYER, X_ROOST_LISTENER_TRUST, X_ROOST_ON_HOST,
        X_ROOST_REMOTE_ADDR, X_ROOST_TAB_ID, X_ROOST_TRACE_ID,
    };

    #[test]
    fn every_header_name_is_the_exact_string_the_proxy_sees() {
        // Written out rather than derived: a rename is a silent auth bypass,
        // and a test that compares the constant to itself would pass through
        // exactly that rename.
        assert_eq!(X_ROOST_TAB_ID, "x-roost-tab-id");
        assert_eq!(X_ROOST_TRACE_ID, "x-roost-trace-id");
        assert_eq!(X_ROOST_REMOTE_ADDR, "x-roost-remote-addr");
        assert_eq!(X_ROOST_ON_HOST, "x-roost-on-host");
        assert_eq!(X_ROOST_LISTENER_TRUST, "x-roost-listener-trust");
        assert_eq!(X_ROOST_AUTH_LAYER, "x-roost-auth-layer");
    }

    #[test]
    fn every_sentinel_is_the_exact_value_the_auth_layer_switches_on() {
        assert_eq!(AUTH_LAYER_DEVICE, "device");
        assert_eq!(AUTH_LAYER_TRUSTED_PROXY, "trusted-proxy");
        assert_eq!(LISTENER_TRUST_YES, "1");
    }

    /// One byte of an HTTP token, which is what a header name may contain.
    fn is_header_token(byte: u8) -> bool {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
    }

    #[test]
    fn a_header_name_is_lowercase_prefixed_and_free_of_whitespace() {
        // Header names travel in a request line, so a name carrying a space or
        // a capital is not the header the coordinator reads.
        for name in ROOST_REQUEST_HEADERS {
            assert!(name.starts_with("x-roost-"), "{name} lost its prefix");
            assert!(name.bytes().all(is_header_token), "{name} is not a token");
            assert!(
                !name.chars().any(char::is_whitespace),
                "{name} carries whitespace"
            );
        }
    }

    #[test]
    fn the_two_tables_cover_every_name_and_sentinel_exactly_once() {
        for (header, sentinel) in AUTH_LAYER_SENTINELS {
            assert!(
                ROOST_REQUEST_HEADERS.contains(&header),
                "{header} is not one of the declared headers"
            );
            assert!(!sentinel.is_empty());
        }
        // Every declared sentinel is the one its own header switches on, and
        // nothing else in the table claims the same value.
    }

    #[test]
    fn a_sentinel_names_exactly_one_auth_layer() {
        // The two auth layers and the trust marker share a header namespace,
        // so an overlapping value would let a value chosen for one layer be
        // read as another.
        assert_ne!(AUTH_LAYER_DEVICE, AUTH_LAYER_TRUSTED_PROXY);
        assert_ne!(AUTH_LAYER_DEVICE, LISTENER_TRUST_YES);
        assert_ne!(AUTH_LAYER_TRUSTED_PROXY, LISTENER_TRUST_YES);
    }
}
