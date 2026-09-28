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
//!
//! NOT here: `terminal-peer-webrtc-v1` and `attachment-transfer-peer-webrtc-v1`.
//! v2 adds them only when its peer owner is supported, and this build's
//! `crate::peer` has no transport behind it.

use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_METADATA_V1,
    CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};

/// The capabilities this worker advertises, in the order the coordinator
/// compares them.
///
/// SORTED, as v2 sorts them: a hello is compared field by field and a
/// reordered list is a different hello.
#[must_use]
pub fn advertised() -> Vec<String> {
    let mut capabilities = vec![
        CAPABILITY_TERMINAL_METADATA_V1.to_owned(),
        CAPABILITY_TERMINAL_VIEW_OWNER_V1.to_owned(),
        CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned(),
    ];
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

    use roost_protocol::versioning::{
        CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
        CAPABILITY_TERMINAL_METADATA_V1, CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
        CAPABILITY_TERMINAL_VIEW_OWNER_V1,
    };

    use super::advertised;

    /// EVERY SPELLING IS THE PROTOCOL'S OWN. A capability is compared as a
    /// string on both sides, so a hand-written spelling that differs by a hyphen
    /// is a capability the coordinator silently never matches — the worker
    /// believes it offered view ownership and the coordinator believes it did
    /// not hear.
    #[test]
    fn every_advertised_name_is_the_protocols_own_spelling() {
        let known = [
            CAPABILITY_TERMINAL_METADATA_V1.to_owned(),
            CAPABILITY_TERMINAL_VIEW_OWNER_V1.to_owned(),
            CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned(),
            CAPABILITY_TERMINAL_PEER_WEBRTC_V1.to_owned(),
            CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1.to_owned(),
        ];
        for capability in advertised() {
            assert!(
                known.contains(&capability),
                "the hello advertises {capability:?}, which is not a capability the \
                 protocol defines, so a coordinator can never match it"
            );
        }
    }

    /// THE LIST IS A PROMISE, and a capability this build cannot serve must not
    /// be in it: the WebRTC carriers have no transport behind `crate::peer`.
    #[test]
    fn a_capability_with_no_collaborator_behind_it_is_not_advertised() {
        let advertised = advertised();
        for unimplemented in [
            CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
            CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1,
        ] {
            assert!(
                !advertised.contains(&unimplemented.to_owned()),
                "the hello advertises {unimplemented:?}, and this build has no WebRTC \
                 carrier behind crate::peer"
            );
        }
    }

    /// A hello is compared field by field, so the list is sorted and free of
    /// duplicates. Two spellings of one capability in one hello is a
    /// coordinator that matches the first and ignores the second.
    #[test]
    fn the_list_is_sorted_and_free_of_duplicates() {
        let advertised = advertised();
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
