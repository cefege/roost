//! The Sync WebSocket handshake every client of the coordinator's sync stream
//! shares: its path, its auth subprotocol, and the capability-negotiation query
//! values.
//!
//! These are wire protocol, not configuration. The coordinator's upgrade path
//! matches on them exactly, so a value changes only with a negotiation bump
//! (`sync_v`) and never in place — a client and a coordinator that disagree here
//! do not fall back to a slower mode, they fail to connect.
//!
//! `SYNC_QUERY_V2` is the one of the four owned by `versioning`, which is where
//! every wire literal that names a version lives.

/// The path the coordinator upgrades for the sync stream.
pub const SYNC_WS_PATH: &str = "/ws/coord-sync";
/// The requested subprotocol every sync client names. The credential follows as
/// a later entry in the same list and is never put in the request URL.
pub const SYNC_AUTH_SUBPROTOCOL: &str = "roost-auth";
/// The query value that selects the v1 sync flow, kept so an older client
/// negotiates down explicitly instead of by accident.
pub const SYNC_QUERY_FLOW_V1: &str = "1";

/// The query value that selects domain generations and socket identity.
pub use crate::versioning::SYNC_QUERY_V2;

/// Every negotiation value understood on the sync upgrade, in the order the
/// coordinator tests them.
pub const SYNC_QUERY_VALUES: [&str; 2] = [SYNC_QUERY_FLOW_V1, SYNC_QUERY_V2];

#[cfg(test)]
mod tests {
    use super::{SYNC_AUTH_SUBPROTOCOL, SYNC_QUERY_FLOW_V1, SYNC_QUERY_VALUES, SYNC_WS_PATH};
    use crate::versioning::SYNC_QUERY_V2;

    #[test]
    fn the_handshake_spells_the_path_and_subprotocol_the_upgrade_matches() {
        // A client that connects to the wrong path gets an HTTP error that
        // looks exactly like a down coordinator, and a client that offers the
        // wrong subprotocol is refused before authentication is attempted.
        assert_eq!(SYNC_WS_PATH, "/ws/coord-sync");
        assert_eq!(SYNC_AUTH_SUBPROTOCOL, "roost-auth");
    }

    #[test]
    fn the_negotiation_values_are_the_strings_a_url_carries() {
        assert_eq!(SYNC_QUERY_FLOW_V1, "1");
        assert_eq!(SYNC_QUERY_V2, "2");
        assert_eq!(SYNC_QUERY_VALUES, ["1", "2"]);
    }

    #[test]
    fn a_query_value_is_a_bare_token_and_a_subprotocol_is_a_bare_token() {
        // Both travel as HTTP tokens: a value with a space or a slash in it
        // cannot survive the request line it is written into.
        for value in SYNC_QUERY_VALUES {
            assert!(!value.is_empty());
            assert!(value.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        }
        assert!(SYNC_WS_PATH.starts_with('/'));
        assert!(!SYNC_AUTH_SUBPROTOCOL.contains(' '));
    }
}
