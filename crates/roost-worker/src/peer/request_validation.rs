//! Bounded identity validation for coordinator-authenticated terminal peer
//! offers: the peer id is a UUID and every other opaque id is present, short
//! and free of control characters. Called by `peer::owner_offer` before any
//! native allocation; it owns no state and never logs an identifier. Ports v2
//! `apps/worker/src/terminal/peer/terminal-peer-request-validation.ts`.

use roost_proto::DLocalTerminalPeerOffer;

const MAX_OPAQUE_ID_BYTES: usize = 128;

pub fn valid_terminal_peer_offer_identity(request: &DLocalTerminalPeerOffer) -> bool {
    is_uuid(&request.peer_id)
        && valid_opaque_id(&request.request_id)
        && valid_opaque_id(&request.connection_generation)
        && valid_opaque_id(&request.grant_id)
        && valid_opaque_id(&request.device_fingerprint)
        && valid_opaque_id(&request.tab_id)
}

/// v2's `/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i`.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let [first, second, third, fourth, fifth] = groups.as_slice() else {
        return false;
    };
    let hex = |group: &str, length: usize| {
        group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    hex(first, 8)
        && hex(second, 4)
        && hex(third, 4)
        && hex(fourth, 4)
        && hex(fifth, 12)
        && matches!(third.as_bytes()[0], b'1'..=b'5')
        && matches!(
            fourth.as_bytes()[0].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}

/// v2 bounds `value.length`, which counts UTF-16 code units, so this does too.
fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.encode_utf16().count() <= MAX_OPAQUE_ID_BYTES
        && !value
            .chars()
            .any(|character| character <= '\u{1f}' || character == '\u{7f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(peer_id: &str, grant_id: &str) -> DLocalTerminalPeerOffer {
        DLocalTerminalPeerOffer {
            request_id: "request".into(),
            connection_generation: "generation".into(),
            grant_id: grant_id.into(),
            peer_id: peer_id.into(),
            device_fingerprint: "device".into(),
            tab_id: "tab".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_peer_id_must_be_a_versioned_uuid_and_ids_must_be_bounded() {
        let uuid = "0f8fad5b-d9cb-469f-a165-70867728950e";
        assert!(valid_terminal_peer_offer_identity(&offer(uuid, "grant")));
        assert!(!valid_terminal_peer_offer_identity(&offer(
            "0f8fad5b-d9cb-069f-a165-70867728950e",
            "grant"
        )));
        assert!(!valid_terminal_peer_offer_identity(&offer(
            "0f8fad5b-d9cb-469f-c165-70867728950e",
            "grant"
        )));
        assert!(!valid_terminal_peer_offer_identity(&offer(uuid, "")));
        assert!(!valid_terminal_peer_offer_identity(&offer(
            uuid,
            "grant\u{7f}"
        )));
        assert!(!valid_terminal_peer_offer_identity(&offer(
            uuid,
            &"g".repeat(129)
        )));
    }
}
