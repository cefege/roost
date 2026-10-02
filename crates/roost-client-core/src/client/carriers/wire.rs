//! The direct carrier's wire: a `DirectCommand` as the `LocalTerminalClientFrame`
//! a worker decodes, and a `LocalTerminalServerFrame` as the typed values the
//! client core folds.
//!
//! Owned by `client::carriers`, called by whatever opens a loopback socket or a
//! WebRTC peer. One wire shape across BOTH carriers (`local_terminal.proto:6`),
//! which is why the peer and the loopback cannot disagree about a frame.
//!
//! Two rules are this file's whole reason to exist. Outbound, every value on the
//! wire comes from the `DirectCommand` the core emitted — never from a store
//! read at send time, which would be a second and later answer to "what was this
//! command". Inbound, a `Ready` and everything after it are distinct values: a
//! carrier that has not authenticated cannot be handed a cell frame, which is
//! the pre-hello rule `local_terminal.proto:18` states.
//!
//! Ports the frame vocabulary of
//! `apps/web/src/store/transport/local-terminal.ts` (`sendFrame`/`receiveFrame`)
//! and `apps/web/src/store/transport/terminal-peer-connection.ts`; the peer lane
//! framing above it is `roost_protocol::terminal_peer::packets`.

use std::collections::BTreeSet;

use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    InputCommand, LocalScrollbackRequest, LocalTerminalClientFrame, LocalTerminalClosed,
    LocalTerminalHello, LocalTerminalReady, LocalTerminalServerFrame, TerminalInputRouteClaim,
    TerminalResyncCommand, TerminalTransportProbe, TerminalViewCommand,
};

use crate::client::carriers::ReadyTuple;
use crate::client::carriers::inbound::DirectInbound;
use crate::client::local::door::LoopbackReady;
use crate::effect::DirectCommand;
use crate::terminal::input::InputOutcome;
use crate::terminal::view::ViewIntent;

/// The `LocalTerminalClientFrame` bytes for one command.
///
/// Every value is the EFFECT's, never a re-read of the store at send time. The
/// effect is the statement "send this, with these contents"; a host that
/// reconstructed its contents when it sent would put a second, later answer to
/// that question on the wire, which is the drift the Sync fences exist to
/// prevent moved one layer out.
pub fn encode_direct_command(command: &DirectCommand) -> Vec<u8> {
    let frame = match command {
        DirectCommand::View {
            session_id,
            view_id,
            intent,
            revision,
        } => {
            // A parked or removed view is an INACTIVE lease with no geometry,
            // the same rendering `client::sync::encode` gives the Sync copy.
            let (cols, rows, active) = match intent {
                ViewIntent::Publish { cols, rows } => (*cols, *rows, true),
                ViewIntent::Park | ViewIntent::Unpublish => (0, 0, false),
            };
            ClientFrame::TerminalView(Box::new(TerminalViewCommand {
                view_id: view_id.clone(),
                session_id: session_id.clone(),
                cols,
                rows,
                revision: *revision,
                active,
                ..Default::default()
            }))
        }
        DirectCommand::Resync {
            session_id,
            view_id,
            stream_id,
            grid_epoch,
            seq,
        } => ClientFrame::TerminalResync(Box::new(TerminalResyncCommand {
            view_id: view_id.clone(),
            session_id: session_id.clone(),
            stream_id: stream_id.clone(),
            grid_epoch: grid_epoch.clone(),
            seq: *seq,
            ..Default::default()
        })),
        DirectCommand::Input {
            session_id,
            view_id,
            input_seq,
            bytes,
            input_route_epoch,
        } => ClientFrame::Input(Box::new(InputCommand {
            session_id: session_id.clone(),
            input_seq: *input_seq,
            data: bytes.clone(),
            view_id: view_id.clone(),
            input_route_epoch: input_route_epoch.clone(),
            ..Default::default()
        })),
        DirectCommand::RouteClaim {
            session_id,
            request_id,
            revision,
            worker_epoch,
            domain_generation,
        } => ClientFrame::InputRouteClaim(Box::new(TerminalInputRouteClaim {
            request_id: request_id.clone(),
            session_id: session_id.clone(),
            revision: *revision,
            domain_generation: *domain_generation,
            worker_epoch: worker_epoch.clone(),
            ..Default::default()
        })),
    };
    wrap(frame)
}

/// The `LocalTerminalHello` bytes: the credential a carrier spends to open.
///
/// `peer_id` and `worker_epoch` are EMPTY on loopback, which is not a default:
/// `client::local::door::admit_ready` reads their absence as the ROLLING-worker
/// compatibility case and refuses a peer that sent neither.
pub fn encode_hello(
    grant_id: &str,
    secret: &str,
    tab_id: &str,
    device_fingerprint: &str,
    peer_id: &str,
    worker_epoch: &str,
) -> Vec<u8> {
    wrap(ClientFrame::Hello(Box::new(LocalTerminalHello {
        grant_id: grant_id.to_owned(),
        secret: secret.to_owned(),
        tab_id: tab_id.to_owned(),
        device_fingerprint: device_fingerprint.to_owned(),
        peer_id: peer_id.to_owned(),
        worker_epoch: worker_epoch.to_owned(),
        ..Default::default()
    })))
}

/// The `LocalScrollbackRequest` bytes for one direct history read.
///
/// The request id is the CLIENT's, because the carrier has no RPC framing to
/// correlate a reply with (`local_terminal.proto:43`).
pub fn encode_scrollback(
    request_id: &str,
    session_id: &str,
    end_row: u64,
    max_rows: u32,
    grid_epoch: &str,
) -> Vec<u8> {
    wrap(ClientFrame::Scrollback(Box::new(LocalScrollbackRequest {
        request_id: request_id.to_owned(),
        session_id: session_id.to_owned(),
        end_row,
        max_rows,
        grid_epoch: grid_epoch.to_owned(),
        ..Default::default()
    })))
}

/// The `TerminalTransportProbe` bytes for one content-free control probe.
///
/// Addressed to one worker by fingerprint, because the worker answers only a
/// probe that names it and the answer's epoch is what proves which process is
/// behind the carrier (`local_terminal.proto:72`).
pub fn encode_transport_probe(request_id: &str, worker_fp: &str) -> Vec<u8> {
    wrap(ClientFrame::TransportProbe(Box::new(
        TerminalTransportProbe {
            request_id: request_id.to_owned(),
            worker_fp: worker_fp.to_owned(),
            ..Default::default()
        },
    )))
}

fn wrap(frame: ClientFrame) -> Vec<u8> {
    LocalTerminalClientFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Why a server frame could not be read at all.
///
/// Distinct from [`DirectInbound::PreHelloFrame`]: that one decoded fine and is
/// out of order, while this one is bytes no `LocalTerminalServerFrame` describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    /// The decoder's own reason.
    pub detail: String,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "direct carrier frame was unreadable: {}",
            self.detail
        )
    }
}

impl std::error::Error for WireError {}

/// Read one `LocalTerminalServerFrame`.
///
/// `authenticated` is the caller's own state, and it is the ONLY thing that
/// decides whether a `Ready` is a handshake completing or a worker repeating one:
/// a second `Ready` on a live carrier is a protocol violation, not a re-admission.
pub fn decode_server_frame(bytes: &[u8], authenticated: bool) -> Result<DirectInbound, WireError> {
    let decoded =
        LocalTerminalServerFrame::decode_from_slice(bytes).map_err(|error| WireError {
            detail: error.to_string(),
        })?;
    let Some(frame) = decoded.frame else {
        return Err(WireError {
            detail: "the frame carried no variant".to_owned(),
        });
    };
    if !authenticated && !matches!(frame, ServerFrame::Ready(_)) {
        return Ok(DirectInbound::PreHelloFrame);
    }
    if authenticated && matches!(frame, ServerFrame::Ready(_)) {
        return Err(WireError {
            detail: "a second Ready arrived on an authenticated carrier".to_owned(),
        });
    }
    Ok(match frame {
        ServerFrame::Ready(ready) => DirectInbound::Ready(ready_of(&ready)),
        ServerFrame::TerminalViewState(state) => DirectInbound::ViewState {
            session_id: state.session_id,
            view_id: state.view_id,
            revision: state.revision,
            accepted: state.status
                == roost_proto::TerminalViewStatus::TERMINAL_VIEW_STATUS_ACCEPTED,
            stream_id: state.stream_id,
            effective_cols: state.effective_cols,
            effective_rows: state.effective_rows,
        },
        ServerFrame::InputAccepted(accepted) => DirectInbound::InputResult {
            session_id: accepted.session_id,
            outcome: InputOutcome::Accepted {
                input_seq: accepted.input_seq,
                written_bytes: accepted.written_bytes,
            },
        },
        ServerFrame::InputRejected(rejected) => DirectInbound::InputResult {
            session_id: rejected.session_id,
            outcome: InputOutcome::Rejected {
                input_seq: rejected.input_seq,
                reason: rejected.reason,
            },
        },
        ServerFrame::InputAmbiguous(ambiguous) => DirectInbound::InputResult {
            session_id: ambiguous.session_id,
            outcome: InputOutcome::Ambiguous {
                input_seq: ambiguous.input_seq,
                written_bytes: ambiguous.written_bytes,
                reason: ambiguous.reason,
            },
        },
        ServerFrame::Closed(closed) => DirectInbound::Closed {
            reason: closed.reason,
        },
        ServerFrame::InputRouteResult(result) => {
            DirectInbound::InputRouteResult(crate::sync::decode::input_route_result_of(*result))
        }
        ServerFrame::TransportProbeResult(result) => DirectInbound::TransportProbeResult(
            crate::sync::decode::transport_probe_result_of(*result),
        ),
        // `cell_grid` IS the grid frame, so its own `session_id` is the
        // session; `cell_grid_chunk` is a wrapper whose session lives in the
        // part it carries, which is what `sync::decode::cell_grid_chunk` reads.
        ServerFrame::CellGrid(frame) => DirectInbound::CellGrid {
            session_id: frame.session_id.clone(),
            frame: *frame,
        },
        ServerFrame::CellGridChunk(chunk) => DirectInbound::CellGridChunk {
            session_id: chunk
                .part
                .as_option()
                .map(|part| part.session_id.clone())
                .unwrap_or_default(),
            chunk: *chunk,
        },
        other => {
            return Err(WireError {
                detail: format!(
                    "the {} arm has no client-side rule on this carrier",
                    arm_name(&other)
                ),
            });
        }
    })
}

fn arm_name(frame: &ServerFrame) -> &'static str {
    match frame {
        ServerFrame::Ready(_) => "ready",
        ServerFrame::TerminalViewState(_) => "terminal_view_state",
        ServerFrame::CellGrid(_) => "cell_grid",
        ServerFrame::CellGridChunk(_) => "cell_grid_chunk",
        ServerFrame::InputAccepted(_) => "input_accepted",
        ServerFrame::InputRejected(_) => "input_rejected",
        ServerFrame::InputAmbiguous(_) => "input_ambiguous",
        ServerFrame::Scrollback(_) => "scrollback",
        ServerFrame::Closed(_) => "closed",
        ServerFrame::InputRouteResult(_) => "input_route_result",
        ServerFrame::TransportProbeResult(_) => "transport_probe_result",
    }
}
fn ready_of(ready: &LocalTerminalReady) -> LoopbackReady {
    LoopbackReady {
        worker_fingerprint: ready.worker_fingerprint.clone(),
        session_ids: BTreeSet::from_iter(ready.session_ids.iter().cloned()),
        socket_generation: ready.socket_generation,
        worker_epoch: ready.worker_epoch.clone(),
        socket_id: ready.socket_id.clone(),
        peer_id: ready.peer_id.clone(),
    }
}

/// The `ReadyTuple` a `Ready` claims, for the peer path's admission check.
///
/// Not for the loopback path: `client::local::door::admit_ready` is the rule
/// there, and it admits a ROLLING worker that answers without an epoch or socket
/// id. This is the peer check, which has no such case because the coordinator
/// bound the offer to one process (`client/carriers/signaling.rs`).
pub fn peer_ready_tuple(ready: &LocalTerminalReady) -> ReadyTuple {
    ReadyTuple {
        worker_fp: ready.worker_fingerprint.clone(),
        worker_epoch: ready.worker_epoch.clone(),
        peer_id: ready.peer_id.clone(),
        socket_generation: ready.socket_generation,
        socket_id: ready.socket_id.clone(),
        session_ids: BTreeSet::from_iter(ready.session_ids.iter().cloned()),
    }
}

/// The `Closed` reason a worker sent, or a host's own when it sent none.
///
/// Empty is a real possibility on the wire, and a log line reading `closed: ` is
/// worse than one naming the socket's own end, so the fallback is supplied by
/// the caller rather than left blank.
pub fn closed_reason(frame: &LocalTerminalClosed, fallback: &str) -> String {
    if frame.reason.is_empty() {
        fallback.to_owned()
    } else {
        frame.reason.clone()
    }
}
