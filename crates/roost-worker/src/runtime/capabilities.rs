//! What this worker's hello tells the coordinator it can be asked for. One
//! named list, called by `runtime::link_serve` on every dial, and by nothing
//! else. The spellings are `roost_protocol::versioning`'s.
//!
//! A capability is a PROMISE: a coordinator that reads one routes traffic here
//! and waits for the answer. Each entry names the collaborator that earns it:
//!  - `terminal-view-owner-v1` — `terminal_view::TerminalViewOwner` owns view
//!    membership, geometry and stream generations for this worker's sessions.
//!  - `terminal-input-route-v1` — `terminal_input::TerminalInputRouteOwner`
//!    answers the coordinator's route claims.
//!  - `terminal-peer-webrtc-v1` / `attachment-transfer-peer-webrtc-v1` —
//!    only when that peer owner's native bootstrap was `ready`
//!    (`peer::DirectPeerSupport`); a disabled or unloadable transport is never
//!    promised.

use roost_protocol::versioning::{
    CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
    CAPABILITY_TERMINAL_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};

use crate::peer::DirectPeerSupport;

/// The capabilities this worker advertises, in the order the coordinator
/// compares them.
///
/// SORTED: a hello is compared field by field and a
/// reordered list is a different hello.
#[must_use]
pub fn advertised(direct: DirectPeerSupport) -> Vec<String> {
    let mut capabilities = vec![
        CAPABILITY_TERMINAL_VIEW_OWNER_V1.to_owned(),
        CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned(),
    ];
    if direct.terminal {
        capabilities.push(CAPABILITY_TERMINAL_PEER_WEBRTC_V1.to_owned());
    }
    if direct.attachment {
        capabilities.push(CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned());
    }
    capabilities.sort();
    capabilities.dedup();
    capabilities
}

#[cfg(test)]
mod tests {
    // A test unwraps the value it is asserting about: a failure there IS the
    // assertion failing, which is what a test wants. The workspace denies
    // unwrap/expect because a panic on a bad value in a running component is a
    // fleet-visible outage, and that reasoning does not reach a test.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::advertised;
    use crate::peer::DirectPeerSupport;
    use roost_protocol::versioning::{
        CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
    };
    const BOTH: DirectPeerSupport = DirectPeerSupport {
        terminal: true,
        attachment: true,
    };

    /// THE LIST IS A PROMISE: a WebRTC carrier is advertised only when its
    /// owner bootstrapped, and each one independently of the other.
    #[test]
    fn a_peer_capability_is_advertised_only_for_a_bootstrapped_owner() {
        let terminal = CAPABILITY_TERMINAL_PEER_WEBRTC_V1.to_owned();
        let attachment = CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned();
        let neither = advertised(DirectPeerSupport::default());
        assert!(!neither.contains(&terminal) && !neither.contains(&attachment));
        let only_terminal = advertised(DirectPeerSupport {
            terminal: true,
            attachment: false,
        });
        assert!(only_terminal.contains(&terminal) && !only_terminal.contains(&attachment));
        let only_attachment = advertised(DirectPeerSupport {
            terminal: false,
            attachment: true,
        });
        assert!(!only_attachment.contains(&terminal) && only_attachment.contains(&attachment));
        let both = advertised(BOTH);
        assert!(both.contains(&terminal) && both.contains(&attachment));
    }

    /// A hello is compared field by field, so the list is sorted and free of
    /// duplicates. Two spellings of one capability in one hello is a
    /// coordinator that matches the first and ignores the second.
    #[test]
    fn the_list_is_sorted_and_free_of_duplicates() {
        let advertised = advertised(BOTH);
        let mut sorted = advertised.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            advertised, sorted,
            "the hello's capability list is in a stable order, because a reordered list is a \
             different hello on the wire"
        );
    }
}
