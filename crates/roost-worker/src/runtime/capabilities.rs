//! What this worker's hello tells the coordinator it can be asked for. One
//! named list, called by `runtime::link_serve` on every dial, and by nothing
//! else. Depends on `roost_protocol::versioning` for the spellings — and on
//! nothing here.
//!
//! WHY IT IS ITS OWN MODULE rather than a literal in the hello. A capability is
//! a PROMISE: a coordinator that reads one believes this worker will answer the
//! traffic it routes here, and a worker that advertises a capability it cannot
//! serve does not fail — it fails a browser, on a command, minutes later, with
//! nothing in the log saying which promise was broken. So the list is named,
//! and each entry names the collaborator that earns it, so adding a capability
//! is a change someone has to justify against a collaborator that exists.
//!
//! WHAT IS NOT HERE, and why, is the more useful half:
//!  - `terminal-view-owner-v1` — a worker advertising this owns terminal view
//!    membership, geometry aggregation and stream generations for its own
//!    sessions. This build has no view owner, so the coordinator's own
//!    `TerminalViewHub` owns them, which is v2's fallback and not a degraded
//!    answer to a claim.
//!  - `terminal-input-route-v1` — the coordinator hands back input routing to a
//!    worker that advertises it. Nothing here routes input yet.
//!  - `terminal-peer-webrtc-v1` and `attachment-transfer-peer-webrtc-v1` — the
//!    direct carriers need a transport `crate::peer` does not have. A browser on
//!    this machine reaches its own PTYs through the door instead.

use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;

/// The capabilities this worker advertises, in the order the coordinator
/// compares them.
///
/// SORTED, and that is not cosmetic: a hello is compared field by field in a
/// golden test and a reordered list is a different hello. The set is tiny enough
/// that sorting costs nothing and removes the question.
#[must_use]
pub fn advertised() -> Vec<String> {
    let mut capabilities = vec![CAPABILITY_TERMINAL_METADATA_V1.to_owned()];
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
        CAPABILITY_TERMINAL_PEER_WEBRTC_V1, CAPABILITY_TERMINAL_VIEW_OWNER_V1,
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
    /// be in it. This is the test that fails the day someone adds a name
    /// without the collaborator behind it — the day the failure is otherwise
    /// invisible until a browser waits on a command nobody serves.
    #[test]
    fn a_capability_with_no_collaborator_behind_it_is_not_advertised() {
        let advertised = advertised();
        for unimplemented in [
            CAPABILITY_TERMINAL_VIEW_OWNER_V1,
            CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
            CAPABILITY_TERMINAL_PEER_WEBRTC_V1,
            CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1,
        ] {
            assert!(
                !advertised.contains(&unimplemented.to_owned()),
                "the hello advertises {unimplemented:?}, and this build has no collaborator \
                 that can serve it: no view owner, no input route owner, and no WebRTC \
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
