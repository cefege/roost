//! The client\'s reader thread: decode frames from the keeper and route each
//! one to whoever is waiting for it, or to the worker\'s event stream. Owned by
//! the keeper client.
//!
//! Split from [`crate::client`] so the request API reads as a request API. A
//! reader thread is the one place a frame arrives from outside a call, and
//! keeping it separate is what makes "who answers this?" answerable by reading
//! one function.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::client_error::ClientError;
use crate::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use crate::frames::SpawnAck;
use crate::payloads::KeeperObservation;

/// The shared state a reader thread needs to answer a pending spawn.
///
/// Behind a mutex because the reader thread and the caller's thread both reach
/// it, and a pending spawn is the only thing either of them owns.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) pending: HashMap<u16, PendingSpawn>,
    pub(crate) keeper: Option<KeeperObservation>,
}

/// A `Spawn` the client is waiting on.
///
/// The deadline lives in the caller's `recv_timeout`, not here: the caller is
/// the only party that can be woken when it expires, and a sweep would have to
/// wake it anyway.
pub(crate) struct PendingSpawn {
    pub(crate) sender: Sender<Result<u32, ClientError>>,
}

/// Read frames until the keeper stops, routing answers to whoever is waiting
/// and everything else to the worker's event stream.
pub(crate) fn read_frames(
    mut stream: UnixStream,
    shared: Arc<Mutex<Shared>>,
    events: Sender<MuxFrame>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut decoder = FrameDecoder::new();
    let mut buffer = vec![0u8; 64 * 1024];
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                for event in decoder.push(&buffer[..read]) {
                    match event {
                        StreamEvent::Frame {
                            frame_type: Some(frame_type),
                            channel_id,
                            payload,
                            ..
                        } => {
                            let frame = MuxFrame {
                                frame_type,
                                channel_id,
                                payload,
                            };
                            let was_an_answer = answer(&shared, &frame);
                            if !was_an_answer {
                                // Not an answer to a pending request, so it is
                                // output, an exit, or a pong. A closed receiver
                                // means the worker has gone; there is nothing
                                // left to deliver it to.
                                if events.send(frame).is_err() {
                                    return;
                                }
                            }
                        }
                        // An unknown tag is skipped rather than fatal: it is
                        // how a newer keeper says it has something this client
                        // predates, and the length is already known.
                        StreamEvent::Frame { .. } => {}
                        StreamEvent::Failed(_) => return,
                    }
                }
            }
            // The read timeout is how the thread notices it should check for
            // work, not an error. A keeper that is simply idle is healthy. It
            // is also the only place a dropped client can be noticed, because
            // the peer will not close anything while this thread holds its dup.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                continue;
            }
            Err(_) => return,
        }
    }
}

/// Hand a frame to whoever is waiting for it, if anyone is.
fn answer(shared: &Mutex<Shared>, frame: &MuxFrame) -> bool {
    let mut guard = match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match frame.frame_type {
        MuxFrameType::SpawnAck => {
            let Some(pending) = guard.pending.get(&frame.channel_id) else {
                return false;
            };
            let pid = frame
                .parse_json::<SpawnAck>()
                .map(|ack| ack.pid)
                .unwrap_or(0);
            // A zero pid is a real answer with a real problem, and reporting it
            // as success would leave the caller holding a channel that was
            // never opened.
            let outcome = if pid == 0 {
                Err(ClientError::SpawnRefused(
                    "the keeper reported no process".into(),
                ))
            } else {
                Ok(pid)
            };
            let _ = pending.sender.send(outcome);
            guard.pending.remove(&frame.channel_id);
            true
        }
        MuxFrameType::SpawnErr => {
            let Some(pending) = guard.pending.get(&frame.channel_id) else {
                return false;
            };
            let reason = frame
                .parse_json::<crate::frames::SpawnErr>()
                .map(|err| err.error)
                .unwrap_or_else(|| "the keeper refused the spawn".to_string());
            let _ = pending.sender.send(Err(ClientError::SpawnRefused(reason)));
            guard.pending.remove(&frame.channel_id);
            true
        }
        _ => false,
    }
}
