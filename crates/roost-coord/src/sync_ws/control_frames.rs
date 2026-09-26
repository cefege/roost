//! Every frame a Sync v2 socket sends on the CONTROL lane: unsequenced,
//! unqueued, and never consuming the acknowledgement window.
//!
//! Owned by the Sync session. The vocabulary is small and closed -- subscribed,
//! domain reset, input accepted/rejected/ambiguous, route result, transport
//! probe result -- and it lives in one file because the reason a control is a
//! control is the same for all of them: it is an ANSWWER to something this
//! socket asked, so it must survive a domain reset that invalidates queued
//! application traffic (`docs/FAILURE-INDEX.md`, "A terminal domain reset is
//! treated as the input fence").
//!
//! WHY THE STAMP IS `delivery_seq = 0`, `domain = UNSPECIFIED`,
//! `domain_generation = 0`. A client that folds a control must be able to tell
//! it apart from application traffic without consulting the window, and a
//! client that compares domain generations must not see a control appear to
//! belong to a generation it just reset.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{
    FirehoseFrame, InputRejected, SyncDomain, SyncDomainResetFrame, SyncSubscribedFrame,
    TerminalInputRouteResult, TerminalTransportProbeResult,
};

/// A domain reset the session performed, with everything the caller's control
/// frame needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetNotice {
    /// Which domain was reset.
    pub domain: SyncDomain,
    /// The generation the reset established. A client ignores every frame from
    /// any other generation.
    pub generation: u64,
    /// The coordinator's reason, carried to the log and to the client's own
    /// decisions.
    pub reason: &'static str,
    /// Whether the domain is still subscribed after the reset.
    pub subscribed: bool,
    /// Whether the terminal lanes were torn down with it.
    pub terminal_sessions_dropped: bool,
}

impl ResetNotice {
    /// The control frame a client must receive for this reset.
    #[must_use]
    pub fn to_frame(&self) -> FirehoseFrame {
        control_frame(FirehoseFrame {
            frame: Some(Frame::DomainReset(Box::new(SyncDomainResetFrame {
                domain: self.domain.into(),
                generation: self.generation,
                reason: self.reason.to_owned(),
                subscribed: self.subscribed,
                __buffa_unknown_fields: Default::default(),
            }))),
            ..FirehoseFrame::default()
        })
    }
}

/// Stamp a frame onto the control lane.
#[must_use]
pub fn control_frame(mut frame: FirehoseFrame) -> FirehoseFrame {
    frame.delivery_seq = 0;
    frame.domain = SyncDomain::Unspecified.into();
    frame.domain_generation = 0;
    frame
}

/// The `subscribed` barrier: this socket's identity, the coordinator's process
/// epoch, and every domain's generation and subscription state.
#[must_use]
pub fn subscribed_frame(
    socket_id: &str,
    process_epoch: &str,
    generations: &[(SyncDomain, u64, bool)],
) -> FirehoseFrame {
    let announced = generations
        .iter()
        .map(
            |(domain, generation, subscribed)| roost_proto::SyncDomainGeneration {
                domain: (*domain).into(),
                generation: *generation,
                subscribed: *subscribed,
                __buffa_unknown_fields: Default::default(),
            },
        )
        .collect();
    control_frame(FirehoseFrame {
        frame: Some(Frame::Subscribed(Box::new(SyncSubscribedFrame {
            socket_id: socket_id.to_owned(),
            process_epoch: process_epoch.to_owned(),
            generations: announced,
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    })
}

/// A refusal of an `input` command, echoing the command's own generation.
///
/// The generation echoed is the one the CLIENT sent, not the domain's current
/// one. The client's correlator matches on `(socketId, sessionId, inputSeq)` and
/// then on the generation the coordinator echoes, so echoing the current
/// generation would make a refused batch look like a batch belonging to a
/// generation the client never asked about
/// (`docs/FAILURE-INDEX.md`, "A terminal domain reset is treated as the input
/// fence").
#[must_use]
pub fn input_rejected_frame(
    session_id: &str,
    input_seq: u64,
    domain_generation: u64,
    reason: &str,
) -> FirehoseFrame {
    control_frame(FirehoseFrame {
        frame: Some(Frame::InputRejected(Box::new(InputRejected {
            session_id: session_id.to_owned(),
            input_seq,
            domain_generation,
            reason: reason.to_owned(),
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    })
}

/// A refusal of an `inputRouteClaim`, as a route result that was not accepted.
#[must_use]
pub fn input_route_refusal_frame(
    request_id: &str,
    session_id: &str,
    revision: u64,
    worker_epoch: &str,
    reason: &str,
) -> FirehoseFrame {
    control_frame(FirehoseFrame {
        frame: Some(Frame::InputRouteResult(Box::new(
            TerminalInputRouteResult {
                request_id: request_id.to_owned(),
                session_id: session_id.to_owned(),
                revision,
                accepted: false,
                latest_revision: 0,
                input_route_epoch: String::new(),
                worker_epoch: worker_epoch.to_owned(),
                reason: reason.to_owned(),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        ..FirehoseFrame::default()
    })
}

/// A refusal of a `terminalTransportProbe`.
///
/// The probe's wire shape has no error field, so the ONLY way to answer "not
/// here" without impersonating a worker is an empty `worker_epoch`: the client's
/// correlator treats an empty epoch as explicitly unsuccessful, and a probe that
/// went unanswered would leave it waiting (`sync-ws-v2-commands.ts:236-251`).
#[must_use]
pub fn transport_probe_refusal_frame(request_id: &str, worker_fp: &str) -> FirehoseFrame {
    control_frame(FirehoseFrame {
        frame: Some(Frame::TerminalTransportProbeResult(Box::new(
            TerminalTransportProbeResult {
                request_id: request_id.to_owned(),
                worker_fp: worker_fp.to_owned(),
                worker_epoch: String::new(),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        ..FirehoseFrame::default()
    })
}

/// The keepalive a long-lived socket sends on a fixed cadence.
#[must_use]
pub fn keepalive_frame(now_ms: u64) -> FirehoseFrame {
    control_frame(FirehoseFrame {
        frame: Some(Frame::Keepalive(Box::new(roost_proto::KeepaliveFrame {
            ts: i64::try_from(now_ms).unwrap_or(i64::MAX),
            __buffa_unknown_fields: Default::default(),
        }))),
        ..FirehoseFrame::default()
    })
}
