//! What this worker's hello tells the coordinator it can be asked for. One
//! named list, called by `runtime::link_serve` on every dial, and by nothing
//! else. Ports v2 `apps/worker/src/transport/coord-link-deps.ts:96-100` and the
//! `additionalCapabilities` of `main.ts:181-185`; the spellings are
//! `roost_protocol::versioning`'s.
//!
//! A capability is a PROMISE: a coordinator that reads one routes traffic here
//! and waits for the answer. Each entry names the collaborator that earns it:
//!  - `terminal-metadata-v1` — the cell emitter stages compact metadata and
//!    the link negotiates it from the hello-ack.
//!  - `terminal-view-owner-v1` — `terminal_view::TerminalViewOwner` owns view
//!    membership, geometry and stream generations for this worker's sessions.
//!  - `terminal-input-route-v1` — `terminal_input::TerminalInputRouteOwner`
//!    answers the coordinator's route claims.
//!  - `terminal-peer-webrtc-v1` / `attachment-transfer-peer-webrtc-v1` —
//!    only when that peer owner's native bootstrap was `ready`
//!    (`peer::DirectPeerSupport`, v2 `boot-local-terminal.ts:173-174`); a
//!    disabled or unloadable transport is never promised.

use roost_protocol::versioning::{
    CAPABILITY_AGENT_TOOLS_V1, CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1,
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_METADATA_V1,
    CAPABILITY_TERMINAL_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};

use crate::peer::DirectPeerSupport;

/// The capabilities this worker advertises, in the order the coordinator
/// compares them.
///
/// SORTED, as v2 sorts them: a hello is compared field by field and a
/// reordered list is a different hello.
#[must_use]
pub fn advertised(direct: DirectPeerSupport) -> Vec<String> {
    let mut capabilities = vec![
        CAPABILITY_TERMINAL_METADATA_V1.to_owned(),
        CAPABILITY_TERMINAL_VIEW_OWNER_V1.to_owned(),
        CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned(),
    ];
    capabilities.push(CAPABILITY_AGENT_TOOLS_V1.to_owned());
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
        CAPABILITY_AGENT_TOOLS_V1, CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1,
        CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_METADATA_V1,
        CAPABILITY_TERMINAL_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
    };
    const BOTH: DirectPeerSupport = DirectPeerSupport {
        terminal: true,
        attachment: true,
    };

    /// EVERY SPELLING IS THE PROTOCOL'S OWN. A capability is compared as a
    /// string on both sides, so a hand-written spelling that differs by a hyphen
    /// is a capability the coordinator silently never matches — the worker
    /// believes it offered view ownership and the coordinator believes it did
    /// not hear.
    #[test]
    fn every_advertised_name_is_the_protocols_own_spelling() {
        let known = [
            CAPABILITY_AGENT_TOOLS_V1.to_owned(),
            CAPABILITY_TERMINAL_METADATA_V1.to_owned(),
            CAPABILITY_TERMINAL_VIEW_OWNER_V1.to_owned(),
            CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned(),
            CAPABILITY_TERMINAL_PEER_WEBRTC_V1.to_owned(),
            CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned(),
        ];
        for capability in advertised(BOTH) {
            assert!(
                known.contains(&capability),
                "the hello advertises {capability:?}, which is not a capability the \
                 protocol defines, so a coordinator can never match it"
            );
        }
    }

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
