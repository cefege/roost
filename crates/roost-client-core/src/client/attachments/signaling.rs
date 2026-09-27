//! WebRTC offer/answer setup for the attachment-specific peer connection. A pure
//! state machine: an offer is settled from local SDP, an answer is admitted only
//! when it names this peer id and this worker epoch. Ported from
//! `attachmentPeerSignaling.ts`. Depends on `roost_protocol::terminal_peer::sdp`
//! for the shared inspection and browser-candidate filter.

use std::fmt;

use roost_protocol::terminal_peer::sdp::{
    TerminalPeerSdpError, filter_browser_terminal_peer_udp_candidates, inspect_terminal_peer_sdp,
};

use super::grant::AttachmentDirectGrant;

/// How long the browser may take to produce an offer at all.
pub const NEGOTIATION_DEADLINE_MS: u64 = 15_000;

/// How long candidate gathering may run before the offer is taken with whatever
/// it has. A short window, because an offer with one host candidate works on a
/// LAN and fails across the internet, while waiting for a STUN answer that
/// never comes costs the user three seconds every time.
pub const ICE_GATHERING_DEADLINE_MS: u64 = 3_000;

/// How long the coordinator's answer, and the browser's adoption of it, may
/// take.
pub const NATIVE_ANSWER_DEADLINE_MS: u64 = 8_000;

/// Where the negotiation has got to. The order IS the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingPhase {
    /// Nothing gathered yet.
    New,
    /// An offer is being produced; local SDP has not been read.
    Gathering,
    /// A filtered, inspected offer is waiting to be sent to the coordinator.
    AwaitingAnswer,
    /// The answer named this peer and this worker epoch, and inspected clean.
    AnswerAdmitted,
    /// The answer is the remote description, and the carrier may authenticate.
    Negotiated,
    /// Torn down. Nothing further is admitted.
    Closed,
}

/// Why a negotiation step was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalingRefusal {
    /// The step does not follow from the phase the conversation is in.
    OutOfOrder {
        phase: SignalingPhase,
        expected: SignalingPhase,
    },
    /// The browser produced no local SDP.
    NoLocalSdp,
    /// The offer survived filtering with no candidate left, so it could not
    /// reach anything.
    NoUsableCandidates,
    /// The answer did not name this peer id and this worker epoch, or named no
    /// SDP at all.
    ResponseMismatch,
    /// The SDP itself is refused by the shared inspector.
    SdpRejected(TerminalPeerSdpError),
}

impl fmt::Display for SignalingRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfOrder { phase, expected } => write!(
                formatter,
                "attachment peer negotiation was in {phase:?}, not {expected:?}"
            ),
            Self::NoLocalSdp => formatter.write_str("attachment peer produced no local SDP"),
            Self::NoUsableCandidates => {
                formatter.write_str("attachment peer offer has no usable candidates")
            }
            Self::ResponseMismatch => {
                formatter.write_str("attachment peer negotiation response was invalid")
            }
            Self::SdpRejected(error) => write!(formatter, "attachment peer SDP refused: {error}"),
        }
    }
}

impl std::error::Error for SignalingRefusal {}

/// The request a host puts to the coordinator's attachment-peer negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerNegotiationRequest {
    pub worker_fp: String,
    pub grant_id: String,
    pub tab_id: String,
    /// The peer id this client minted for this attempt.
    pub peer_id: String,
    pub offer_sdp: String,
    pub worker_epoch: String,
}

/// The coordinator's answer, before it is checked against this attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerNegotiationResponse {
    pub peer_id: String,
    pub answer_sdp: String,
    pub worker_epoch: String,
}

/// One attachment peer's gathering and signaling state.
///
/// It holds no SDP: the host owns the peer connection, reads `localDescription`
/// and hands the string here, and this decides whether that string is one this
/// negotiation may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerSignaling {
    stun_urls: Vec<String>,
    phase: SignalingPhase,
}

impl AttachmentPeerSignaling {
    /// A fresh negotiation that may use `stun_urls` and nothing else.
    #[must_use]
    pub fn new(stun_urls: Vec<String>) -> Self {
        Self {
            stun_urls,
            phase: SignalingPhase::New,
        }
    }

    /// Where the conversation has got to.
    #[must_use]
    pub fn phase(&self) -> SignalingPhase {
        self.phase
    }

    /// The ICE servers the peer connection is configured with. The transport
    /// policy is always `all`, and this returns nothing else to configure: a
    /// peer that gathered only host candidates and then filtered them has no
    /// route at all.
    #[must_use]
    pub fn ice_servers(&self) -> &[String] {
        &self.stun_urls
    }

    /// Begin gathering, which is the only thing that may follow a fresh
    /// negotiation.
    pub fn begin_gathering(&mut self) -> Result<(), SignalingRefusal> {
        self.at(SignalingPhase::New)?;
        self.phase = SignalingPhase::Gathering;
        Ok(())
    }

    /// Take the browser's local SDP as the offer, and return the filtered SDP
    /// to send.
    ///
    /// Filtering happens here, before the offer crosses to the coordinator, and
    /// the candidate count is checked after it: an offer whose only candidates
    /// were browser-generated TCP ones is an offer that cannot be connected to,
    /// and discovering that on the far side wastes the whole negotiation.
    pub fn settle_offer(&mut self, local_sdp: &str) -> Result<String, SignalingRefusal> {
        self.at(SignalingPhase::Gathering)?;
        if local_sdp.is_empty() {
            return Err(SignalingRefusal::NoLocalSdp);
        }
        let offer = filter_browser_terminal_peer_udp_candidates(local_sdp);
        let metadata = inspect_terminal_peer_sdp(&offer).map_err(SignalingRefusal::SdpRejected)?;
        if metadata.candidate_count == 0 {
            return Err(SignalingRefusal::NoUsableCandidates);
        }
        self.phase = SignalingPhase::AwaitingAnswer;
        Ok(offer)
    }

    /// The request the host puts to the coordinator, naming the exact grant
    /// this negotiation is for.
    #[must_use]
    pub fn negotiation_request(
        &self,
        grant: &AttachmentDirectGrant,
        peer_id: &str,
        offer_sdp: &str,
    ) -> AttachmentPeerNegotiationRequest {
        AttachmentPeerNegotiationRequest {
            worker_fp: grant.request.worker_fp.clone(),
            grant_id: grant.grant_id.clone(),
            tab_id: grant.tab_id.clone(),
            peer_id: peer_id.to_owned(),
            offer_sdp: offer_sdp.to_owned(),
            worker_epoch: grant.worker_epoch.clone(),
        }
    }

    /// Check the coordinator's answer against this attempt, and return the SDP
    /// to apply.
    ///
    /// Three things must agree before any byte may cross: the peer id this
    /// client minted, the worker epoch the grant names, and the SDP's own
    /// shape. An answer that fails any of them belongs to a negotiation that is
    /// not this one, and applying it would authenticate this upload to a
    /// stranger's peer.
    pub fn admit_answer(
        &mut self,
        peer_id: &str,
        grant: &AttachmentDirectGrant,
        response: &AttachmentPeerNegotiationResponse,
    ) -> Result<String, SignalingRefusal> {
        self.at(SignalingPhase::AwaitingAnswer)?;
        if response.peer_id != peer_id
            || response.worker_epoch != grant.worker_epoch
            || response.answer_sdp.is_empty()
        {
            return Err(SignalingRefusal::ResponseMismatch);
        }
        inspect_terminal_peer_sdp(&response.answer_sdp).map_err(SignalingRefusal::SdpRejected)?;
        self.phase = SignalingPhase::AnswerAdmitted;
        Ok(response.answer_sdp.clone())
    }

    /// The remote description is applied, so the carrier may authenticate.
    pub fn finish(&mut self) -> Result<(), SignalingRefusal> {
        self.at(SignalingPhase::AnswerAdmitted)?;
        self.phase = SignalingPhase::Negotiated;
        Ok(())
    }

    /// Tear the negotiation down. Idempotent, because a close after a failure
    /// and a second close are the same call.
    pub fn close(&mut self) {
        self.phase = SignalingPhase::Closed;
    }

    /// Refuse a step that does not follow from where the conversation is. The
    /// phase only moves on success, so a refused step leaves the negotiation
    /// exactly where it was and the caller may close it deliberately.
    fn at(&self, expected: SignalingPhase) -> Result<(), SignalingRefusal> {
        if self.phase == expected {
            return Ok(());
        }
        Err(SignalingRefusal::OutOfOrder {
            phase: self.phase,
            expected,
        })
    }
}
