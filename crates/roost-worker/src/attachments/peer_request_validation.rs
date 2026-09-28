//! Bounded identity validation of an attachment peer offer before any native
//! allocation. The coordinator already authenticated the request; this refuses
//! malformed opaque fields without logging SDP, grant material or attachment
//! metadata. Called by `peer_owner`. Ports
//! `apps/worker/src/attachments/attachment-peer-request-validation.ts`.

use roost_proto::DLocalAttachmentPeerOffer;

const MAX_OPAQUE_ID_BYTES: usize = 128;

pub fn valid_attachment_peer_offer_identity(request: &DLocalAttachmentPeerOffer) -> bool {
    is_uuid(&request.peer_id)
        && valid_opaque_id(&request.request_id)
        && valid_opaque_id(&request.connection_generation)
        && valid_opaque_id(&request.grant_id)
        && valid_opaque_id(&request.device_fingerprint)
        && valid_opaque_id(&request.tab_id)
}

/// v2's `/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu`.
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        14 => (b'1'..=b'5').contains(byte),
        19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
        _ => byte.is_ascii_hexdigit(),
    })
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPAQUE_ID_BYTES
        && !value
            .chars()
            .any(|character| character <= '\u{1f}' || character == '\u{7f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> DLocalAttachmentPeerOffer {
        DLocalAttachmentPeerOffer {
            request_id: "request".to_owned(),
            connection_generation: "generation".to_owned(),
            grant_id: "grant".to_owned(),
            peer_id: "11111111-1111-4111-8111-111111111111".to_owned(),
            device_fingerprint: "c".repeat(64),
            tab_id: "tab".to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_a_versioned_uuid_peer_and_bounded_opaque_ids() {
        assert!(valid_attachment_peer_offer_identity(&offer()));
        let upper = DLocalAttachmentPeerOffer {
            peer_id: "11111111-1111-4111-A111-11111111111F".to_owned(),
            ..offer()
        };
        assert!(valid_attachment_peer_offer_identity(&upper));
    }

    #[test]
    fn refuses_an_unversioned_peer_an_empty_or_oversized_id_and_control_characters() {
        let unversioned = DLocalAttachmentPeerOffer {
            peer_id: "11111111-1111-6111-8111-111111111111".to_owned(),
            ..offer()
        };
        assert!(!valid_attachment_peer_offer_identity(&unversioned));
        let empty = DLocalAttachmentPeerOffer {
            tab_id: String::new(),
            ..offer()
        };
        assert!(!valid_attachment_peer_offer_identity(&empty));
        let oversized = DLocalAttachmentPeerOffer {
            grant_id: "g".repeat(MAX_OPAQUE_ID_BYTES + 1),
            ..offer()
        };
        assert!(!valid_attachment_peer_offer_identity(&oversized));
        let control = DLocalAttachmentPeerOffer {
            request_id: "req\u{7f}".to_owned(),
            ..offer()
        };
        assert!(!valid_attachment_peer_offer_identity(&control));
    }
}
