//! One worker connection, served to completion: the read/drain loop, the
//! framing, and the reason the connection ended.
//!
//! Split out of `server.rs` because those are two concerns sharing one `impl`:
//! what binds a socket and hands it here belongs to the endpoint, and what a
//! CONNECTED socket does with frames, PTY output and a shutdown request belongs
//! to the loop. The drain tick that governs how fast output reaches a reader
//! lives here with the loop, because that is the thing it governs — and
//! `crates/roost-worker/src/keeper_pool/dispatch.rs` reads this file's copy of
//! it to size the worker's own coalesce ceiling.
//!
//! The methods stay on `Server` rather than becoming a type of their own so a
//! caller keeps one way to serve a connection; a child module can `impl` its
//! parent because a private field is visible to the module's descendants.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::codec::{CodecError, FrameDecoder, MuxFrame, StreamEvent};
use crate::input_queue::ConnectionWriter;

use super::{ConnectionEnd, DRAIN_LIMIT_BYTES, OUTPUT_TICK, READ_BUFFER_BYTES, Server};

impl Server {
    /// A payload the keeper cannot frame: logged with its cause, and reported
    /// as the end of a connection that can no longer carry correct output.
    fn unframeable(err: CodecError) -> ConnectionEnd {
        tracing::error!("keeper: a payload could not be framed: {err}");
        ConnectionEnd::UnframeablePayload
    }

    /// Serve a single connection to completion, reporting why it ended. Its one writer is
    /// shared with every input lane; detaching drops what the departed connection left unstarted.
    pub fn serve_one(&mut self, stream: UnixStream) -> ConnectionEnd {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        let Ok(writer) = ConnectionWriter::for_stream(&stream).map(Arc::new) else {
            return ConnectionEnd::WorkerUnreachable;
        };
        let sink: Arc<ConnectionWriter> = Arc::clone(&writer);
        self.keeper.attach_input_results(sink);
        let end = self.serve_frames(stream, &writer);
        self.keeper.detach_input_results();
        end
    }

    fn serve_frames(&mut self, mut stream: UnixStream, writer: &ConnectionWriter) -> ConnectionEnd {
        let mut decoder = FrameDecoder::new();
        let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
        let mut stopping = false;
        // THE TICK IS A DEADLINE, NOT A WAIT. Two earlier shapes both lost frames
        // a reader could see were missing: a blocking read of `READ_POLL`
        // (100 ms) followed by a sleep of `OUTPUT_TICK` (16 ms), which drained
        // once per 116 ms; and a flat `OUTPUT_TICK` read timeout with no sleep,
        // whose period is `tick + the work the turn did` — measured at 20 ms.
        // A terminal's frame rate IS this period, so both cost frames.
        //
        // Bounding each read by what is LEFT of the next tick boundary puts the
        // period on the grid. That boundary is CHASED from the previous one
        // rather than re-derived from this turn's start, because `SO_RCVTIMEO`
        // on a UnixStream rounds: a 16 ms request measures 20 ms here, so a
        // re-anchored turn charges that 4 ms on every pass and the drain runs
        // at ~45 chunk groups/s rather than the 62.5 the tick claims. Chasing
        // absorbs the rounding into the shorter next read instead.
        let mut deadline = std::time::Instant::now() + OUTPUT_TICK;

        loop {
            // THE GRID IS CHASED, NOT RE-ANCHORED, and that is the whole point.
            //
            // A turn that ends EARLY must not hand the next one a full tick,
            // because the loop's period is the read's cost: `SO_RCVTIMEO` on a
            // UnixStream rounds, and a 16 ms request measures 20 ms on this
            // host (200 samples, p10 19.86 ms). Re-anchoring to the turn's own
            // start therefore charges that 4 ms of rounding to every pass, and
            // the drain runs at ~45 chunk groups/s instead of the 62.5 the tick
            // claims — which is a 40% loss of terminal frames, measured against
            // the oracle's own number.
            //
            // Chasing the PREVIOUS deadline absorbs the rounding instead: a turn
            // that ran long asks for only what is left to the next boundary, so
            // the period lands on the grid rather than above it. The overrun
            // guard stays, because a turn that blew past its deadline entirely
            // must not chase a backlog of boundaries it can no longer reach.
            let turn_start = std::time::Instant::now();
            deadline += OUTPUT_TICK;
            if turn_start > deadline {
                deadline = turn_start + OUTPUT_TICK;
            }
            // The tick first, so output flows even while nothing is arriving.
            // A request-driven loop would show nothing until the user typed.
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

            // The read is bounded by what is LEFT of this turn's tick.
            let _ = stream.set_read_timeout(Some(
                deadline.saturating_duration_since(std::time::Instant::now()),
            ));

            let read = match stream.read(&mut buffer) {
                Ok(0) => return ConnectionEnd::ClientDisconnected,
                Ok(read) => read,
                // NO SLEEP HERE. The read already waited out the tick, so
                // sleeping again paces the drain at the SUM of the two waits
                // rather than at either one. Measured on a real daemon: 120 ms
                // of silence per frame with the sleep, 20 ms without.
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(_) => return ConnectionEnd::ClientDisconnected,
            };

            for event in decoder.push(&buffer[..read]) {
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
                            return ConnectionEnd::WorkerUnreachable;
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
                                return if refused {
                                    ConnectionEnd::ShutdownIfEmptyRefused
                                } else {
                                    ConnectionEnd::ShutdownIfEmptyAccepted
                                };
                            }
                            _ => {}
                        }
                    }
                    // The stream violated the protocol. There is no safe
                    // resynchronisation point, so the connection ends.
                    StreamEvent::Failed(_) => return ConnectionEnd::ProtocolViolation,
                }
            }
            if stopping {
                return ConnectionEnd::ShutdownRequestedWithChannels;
            }
        }
    }
}
