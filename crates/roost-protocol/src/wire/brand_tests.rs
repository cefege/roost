//! The branded id tests, split out of `brand.rs` so the module carrying the
//! `Display` impls and their shape checks stays under the size cap.

use super::brand::*;


const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn a_fingerprint_is_lowercase_hex_of_exactly_sixty_four() {
    assert!(WorkerFp::try_from(FINGERPRINT).is_ok());
    assert!(WorkerFp::try_from(FINGERPRINT.to_uppercase()).is_err());
    assert!(WorkerFp::try_from(&FINGERPRINT[..63]).is_err());
    assert!(WorkerFp::try_from(format!("{FINGERPRINT}0")).is_err());
}

#[test]
fn a_uuid_id_accepts_either_case_and_rejects_a_fingerprint() {
    assert!(SessionId::try_from(SESSION).is_ok());
    assert!(SessionId::try_from(SESSION.to_uppercase()).is_ok());
    assert!(SessionId::try_from("not-a-uuid").is_err());
    // The brands are only distinct if a worker fingerprint is not a session
    // id, even though both passed *a* shape check.
    assert!(SessionId::try_from(FINGERPRINT).is_err());
    assert!(WorkspaceId::try_from(FINGERPRINT).is_err());
    assert!(TaskId::try_from(FINGERPRINT).is_err());
    assert!(McpRelayId::try_from(FINGERPRINT).is_err());
}

#[test]
fn a_channel_is_a_nonnegative_u32_because_the_proto_carries_one() {
    assert_eq!(ChannelId::try_from(0).unwrap().as_u32(), 0);
    assert_eq!(
        ChannelId::try_from(u32::MAX as i64).unwrap().as_u32(),
        u32::MAX
    );
    assert!(ChannelId::try_from(-1).is_err());
    assert!(ChannelId::try_from(u32::MAX as i64 + 1).is_err());
}

#[test]
fn a_trace_id_needs_at_least_eight_hex_characters() {
    assert!(TraceId::try_from("deadbeef").is_ok());
    assert!(TraceId::try_from("deadbee").is_err());
    assert!(TraceId::try_from("deadbeeg").is_err());
}
