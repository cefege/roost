//! One worker connection, served to completion: the authentication every
//! connection must pass first, the event loop that forwards PTY output and
//! answers frames, the framing, and the reason the connection ended.
//!
//! Split out of `server.rs` because those are two concerns sharing one `impl`:
//! what binds a socket and hands it here belongs to the endpoint, and what a
//! CONNECTED socket does with frames, PTY output and a shutdown request belongs
//! to the loop. The methods stay on `Server` rather than becoming a type of
//! their own so a caller keeps one way to serve a connection; a child module
//! can `impl` its parent because a private field is visible to its descendants.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use crate::codec::{CodecError, FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use crate::input_queue::ConnectionWriter;
use crate::payloads::KeeperHelloRequest;

use super::{ConnectionEnd, DRAIN_LIMIT_BYTES, OUTPUT_TICK, READ_BUFFER_BYTES, Server};

/// The most a connection may send before its `Hello` verifies (v2
/// `LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES`). A real `Hello` is a few hundred
/// bytes; anything near this is a peer filling the keeper's memory.
pub const UNAUTHENTICATED_MAX_BYTES: usize = 64 * 1024;

/// How long an accepted connection has to authenticate (v2
/// `LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS`). The keeper serves one
/// connection at a time, so this is also the longest a peer that never
/// authenticates can keep a worker waiting in the listen backlog.
pub const UNAUTHENTICATED_TIMEOUT: Duration = Duration::from_secs(2);

/// How many wake-ups may wait for the loop. Bounded so a worker that outruns
/// the loop is held back by the socket rather than by the keeper's memory; the
/// reader thread blocks on a full queue and stops reading.
const EVENT_QUEUE_DEPTH: usize = 8;

/// How often the socket reader re-checks that its connection is still served.
/// Only a backstop: ending the connection shuts the socket's read side, which
/// wakes a blocked read at once.
const READER_STOP_POLL: Duration = Duration::from_millis(100);

/// What wakes the connection loop.
enum LoopEvent {
    /// Bytes the worker sent, in order.
    Received(Vec<u8>),
    /// The worker closed its end, or the socket failed.
    Closed,
    /// A channel's reader handed over output.
    Output,
}

impl Server {
    /// A payload the keeper cannot frame: logged with its cause, and reported
    /// as the end of a connection that can no longer carry correct output.
    fn unframeable(err: CodecError) -> ConnectionEnd {
        tracing::error!("keeper: a payload could not be framed: {err}");
        ConnectionEnd::UnframeablePayload
    }

    /// Serve a single connection to completion, reporting why it ended. Once it authenticates, its
    /// one writer is shared with every input lane; detaching drops what it left unstarted.
    pub fn serve_one(&mut self, stream: UnixStream) -> ConnectionEnd {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        let Ok(writer) = ConnectionWriter::for_stream(&stream).map(Arc::new) else {
            return ConnectionEnd::WorkerUnreachable;
        };
        let end = self.serve_frames(stream, &writer);
        self.keeper.detach_input_results();
        end
    }

    /// Wire the socket reader and the PTY readers into one queue, serve it, and
    /// take both down again before returning, so the socket is closed when the
    /// connection is reported ended.
    fn serve_frames(
        &mut self,
        stream: UnixStream,
        writer: &Arc<ConnectionWriter>,
    ) -> ConnectionEnd {
        let deadline = Instant::now() + UNAUTHENTICATED_TIMEOUT;
        let (events, inbox) = std::sync::mpsc::sync_channel(EVENT_QUEUE_DEPTH);
        let stopped = Arc::new(AtomicBool::new(false));
        let reader = stream
            .try_clone()
            .and_then(|socket| spawn_socket_reader(socket, events.clone(), Arc::clone(&stopped)));
        let reader = match reader {
            Ok(reader) => reader,
            Err(err) => {
                tracing::error!("keeper: the connection's reader could not start: {err}");
                return ConnectionEnd::WorkerUnreachable;
            }
        };
        // `try_send`: the waker runs on a PTY reader thread, which must never
        // wait on this loop. A full queue already guarantees a turn, and every
        // turn drains every channel.
        self.keeper
            .output_signal
            .attach(Box::new(move || drop(events.try_send(LoopEvent::Output))));

        let end = self.serve_authenticated(&inbox, writer, deadline);

        self.keeper.output_signal.detach();
        stopped.store(true, Ordering::Release);
        // Dropping the queue fails a reader blocked on a full one; shutting the
        // read side wakes one blocked in `read`.
        drop(inbox);
        let _ = stream.shutdown(std::net::Shutdown::Read);
        if reader.join().is_err() {
            tracing::error!("keeper: the connection's reader panicked");
        }
        end
    }

    /// Authenticate, then serve. Input results reach this connection only once
    /// it has proved the capability, and frames that arrived behind its `Hello`
    /// in the same read are answered before the loop's first turn.
    fn serve_authenticated(
        &mut self,
        inbox: &Receiver<LoopEvent>,
        writer: &Arc<ConnectionWriter>,
        deadline: Instant,
    ) -> ConnectionEnd {
        let mut decoder = FrameDecoder::new();
        let behind_hello = match self.authenticate(inbox, writer, &mut decoder, deadline) {
            Ok(events) => events,
            Err(end) => return end,
        };
        let sink: Arc<ConnectionWriter> = Arc::clone(writer);
        self.keeper.attach_input_results(sink);
        if let Some(end) = self.answer_events(behind_hello, writer) {
            return end;
        }
        self.forward_until_end(inbox, writer, &mut decoder)
    }

    /// THE FIRST FRAME MUST BE A `Hello` ON THE CONTROL LANE WHOSE CAPABILITY
    /// VERIFIES, sent inside `UNAUTHENTICATED_TIMEOUT` and
    /// `UNAUTHENTICATED_MAX_BYTES` (v2 `multiplexed-main.ts:170-264`). Until
    /// then nothing is drained, reaped or written: PTY output only wakes this
    /// wait and is left for the first authenticated turn, because a peer that
    /// has not proved the capability must not read a byte of any terminal.
    /// `Ok` holds whatever was decoded behind the `Hello`.
    fn authenticate(
        &mut self,
        inbox: &Receiver<LoopEvent>,
        writer: &ConnectionWriter,
        decoder: &mut FrameDecoder,
        deadline: Instant,
    ) -> Result<Vec<StreamEvent>, ConnectionEnd> {
        let mut received_bytes = 0_usize;
        loop {
            if let Some(cause) = self.poll_exit() {
                return Err(cause.into());
            }
            let Some(left) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                return Err(refused("timeout"));
            };
            let received = match inbox.recv_timeout(left.min(OUTPUT_TICK)) {
                Ok(LoopEvent::Received(received)) => received,
                Ok(LoopEvent::Output) | Err(RecvTimeoutError::Timeout) => continue,
                Ok(LoopEvent::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(ConnectionEnd::ClientDisconnected);
                }
            };
            received_bytes = received_bytes.saturating_add(received.len());
            if received_bytes > UNAUTHENTICATED_MAX_BYTES {
                return Err(refused("too_many_bytes"));
            }
            let mut events = decoder.push(&received).into_iter();
            let Some(first) = events.next() else {
                continue;
            };
            let request = opening_hello(first)?;
            if !self.capability.verify(&request.capability) {
                return Err(refused("bad_capability"));
            }
            let response = self.keeper.hello_response(&request);
            let answer =
                MuxFrame::json(MuxFrameType::HelloResp, 0, &response).map_err(Self::unframeable)?;
            if writer.write_frames(&[answer]).is_err() {
                return Err(ConnectionEnd::WorkerUnreachable);
            }
            tracing::info!(
                peer_pid = ?request.pid,
                channels = response.bindings.len(),
                "keeper: a connection authenticated"
            );
            return Ok(events.collect());
        }
    }

    /// THE LOOP FORWARDS OUTPUT AS IT ARRIVES. Every turn drains every
    /// channel, and a turn starts on any of three wakes: a channel's reader
    /// handed over output, the worker sent bytes, or a tick passed with
    /// neither. So a keystroke's echo leaves the keeper as soon as the PTY
    /// produces it, as v2's data callback did, and the tick only bounds how
    /// long an idle connection goes without a drain and an exit check — the
    /// watch's signal flag and socket check included, so a keeper serving a
    /// worker still stops on SIGTERM or a deleted socket.
    ///
    /// The signal is taken BEFORE the drain: a chunk handed over after the take
    /// raises again and wakes the next turn, and one handed over before it is
    /// in this drain.
    ///
    /// FAIRNESS IS PER TURN. A drain takes at most `DRAIN_LIMIT_BYTES` from each
    /// channel, so one flooding program cannot starve the others or exceed the
    /// frame bound. What its limit held back raises no new signal, so a
    /// backlogged turn does not wait at all: it takes whatever the worker
    /// already sent and comes straight round to drain again.
    fn forward_until_end(
        &mut self,
        inbox: &Receiver<LoopEvent>,
        writer: &ConnectionWriter,
        decoder: &mut FrameDecoder,
    ) -> ConnectionEnd {
        loop {
            if let Some(cause) = self.poll_exit() {
                return cause.into();
            }
            self.keeper.output_signal.take();
            let output = match self.keeper.drain_output(DRAIN_LIMIT_BYTES) {
                Ok(output) => output,
                Err(err) => return Self::unframeable(err),
            };
            if writer.write_frames(&output).is_err() {
                return ConnectionEnd::WorkerUnreachable;
            }
            for exit in match self.keeper.reap_exited() {
                Ok(exits) => exits,
                Err(err) => return Self::unframeable(err),
            } {
                if writer.write_frames(&[exit]).is_err() {
                    return ConnectionEnd::WorkerUnreachable;
                }
            }

            let wait = if self.keeper.output_backlogged() {
                Duration::ZERO
            } else {
                OUTPUT_TICK
            };
            let received = match inbox.recv_timeout(wait) {
                Ok(LoopEvent::Received(received)) => received,
                Ok(LoopEvent::Output) | Err(RecvTimeoutError::Timeout) => continue,
                Ok(LoopEvent::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    return ConnectionEnd::ClientDisconnected;
                }
            };
            if let Some(end) = self.answer_events(decoder.push(&received), writer) {
                return end;
            }
        }
    }

    /// Answer what the worker sent, in order; `Some` ends the connection.
    fn answer_events(
        &mut self,
        events: Vec<StreamEvent>,
        writer: &ConnectionWriter,
    ) -> Option<ConnectionEnd> {
        let mut stopping = false;
        for event in events {
            match event {
                StreamEvent::Frame {
                    frame_type,
                    channel_id,
                    payload,
                    ..
                } => {
                    let Some(frame_type) = frame_type else {
                        // A tag this build predates. The length was already
                        // read, so skipping it is safe, and skipping is
                        // better than dropping a connection over a frame the
                        // client will simply not send again.
                        continue;
                    };
                    let frame = MuxFrame {
                        frame_type,
                        channel_id,
                        payload,
                    };
                    let replies = self.keeper.handle(&frame);
                    if writer.write_frames(&replies).is_err() {
                        return Some(ConnectionEnd::WorkerUnreachable);
                    }
                    match frame.frame_type {
                        crate::codec::MuxFrameType::Shutdown => {
                            stopping = true;
                        }
                        crate::codec::MuxFrameType::ShutdownIfEmpty => {
                            // The answer is what decides, not the request:
                            // a refusal means the keeper stays up, and
                            // acting on the request instead would retire a
                            // keeper that is holding live PTYs.
                            let refused = replies.iter().any(|reply| {
                                reply.frame_type
                                    == crate::codec::MuxFrameType::ShutdownIfEmptyReject
                            });
                            return Some(if refused {
                                ConnectionEnd::ShutdownIfEmptyRefused
                            } else {
                                ConnectionEnd::ShutdownIfEmptyAccepted
                            });
                        }
                        _ => {}
                    }
                }
                // The stream violated the protocol. There is no safe
                // resynchronisation point, so the connection ends.
                StreamEvent::Failed(_) => return Some(ConnectionEnd::ProtocolViolation),
            }
        }
        stopping.then_some(ConnectionEnd::ShutdownRequestedWithChannels)
    }
}

/// The `Hello` a connection must open with, or the refusal of a connection
/// that opened with anything else.
fn opening_hello(first: StreamEvent) -> Result<KeeperHelloRequest, ConnectionEnd> {
    let StreamEvent::Frame {
        frame_type: Some(MuxFrameType::Hello),
        channel_id: 0,
        payload,
        ..
    } = first
    else {
        return Err(refused("not_hello"));
    };
    serde_json::from_slice(&payload).map_err(|_| refused("unreadable_hello"))
}

/// A connection refused before it authenticated. The presented capability is
/// never logged: a near miss in a log is most of the secret.
fn refused(reason: &'static str) -> ConnectionEnd {
    tracing::warn!(
        reason,
        "keeper: a connection was refused before it authenticated"
    );
    ConnectionEnd::NotAuthenticated
}

/// Read the worker's bytes on their own thread, so the loop can wait on the
/// socket and on PTY output at once. Reports a close once and returns; a
/// read that times out only re-checks `stopped`.
fn spawn_socket_reader(
    mut socket: UnixStream,
    events: SyncSender<LoopEvent>,
    stopped: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    socket.set_read_timeout(Some(READER_STOP_POLL))?;
    std::thread::Builder::new()
        .name("roost-keeper-socket".to_owned())
        .spawn(move || {
            let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
            loop {
                let event = match socket.read(&mut buffer) {
                    Ok(0) => LoopEvent::Closed,
                    Ok(read) => LoopEvent::Received(buffer[..read].to_vec()),
                    Err(err)
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        continue;
                    }
                    Err(_) => LoopEvent::Closed,
                };
                let closed = matches!(event, LoopEvent::Closed);
                if events.send(event).is_err() || closed {
                    return;
                }
            }
        })
}
