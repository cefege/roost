//! The worker's side of the keeper socket: connect, negotiate, and route
//! frames. Owned by the worker, and built from the same codec the daemon
//! serves, because one crate owns the protocol on both ends.
//!
//! The interesting part is what happens when the keeper is unhealthy. A
//! keeper that accepts a `Spawn` and never answers is the 2026-06-22 incident:
//! the worker's RPC hangs with no trail and no operator signal. Every wait here
//! is therefore bounded and every failure is reported, because a silent hang
//! costs an operator an afternoon and a logged timeout costs them a line.

use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client_error::ClientError;
use crate::client_io::{PendingSpawn, Shared};
use crate::codec::{MuxFrame, MuxFrameType};
use crate::frames::{ListChannelsResp, ShellSpec, SpawnRequest};
use crate::payloads::{KeeperFeature, KeeperHelloRequest, KeeperObservation, PtyInRequest};

/// How long a healthy keeper has to acknowledge a spawn.
///
/// A healthy keeper acks in well under 100ms. The slack is deliberate: a loaded
/// machine is not a wedged keeper, and killing a keeper that was about to
/// answer costs every live PTY on it.
pub const SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(8);

/// Establish a connection to a keeper, retrying until the deadline, and the
/// wait policy that deadline is expressed in.
pub use crate::client_connect::{CONNECT_RETRY_TIMEOUT, DEADLINE_TICK, HELLO_TIMEOUT, connect};

/// A connected keeper.
pub struct KeeperClient {
    pub(crate) path: PathBuf,
    write_half: Mutex<UnixStream>,
    shared: Arc<Mutex<Shared>>,
    /// Frames the keeper sent that were not answers to a request: PTY output,
    /// exits, pongs. The worker drains this.
    pub(crate) events: Receiver<MuxFrame>,
    /// Frames a control wait pulled off `events` that were not the answer, held
    /// until the worker asks for them.
    ///
    /// This buffer is the whole of the fix for the frame loss the reader thread
    /// interleaves. `events` is ONE channel carrying both PTY output and control
    /// replies, because the keeper writes both on the same socket from the same
    /// connection loop — `server.rs` drains `PtyOut` and writes it there. A wait
    /// that pulled a `PtyOut` and dropped it would lose terminal output on every
    /// round-trip, and a resize drag is sixty round-trips a second. v2 kept the
    /// two paths disjoint by routing every frame by tag in a single loop; here
    /// the disjointness is this buffer.
    pub(crate) deferred: Mutex<VecDeque<MuxFrame>>,
    /// Set on drop so the reader thread stops. A reader that only noticed a
    /// closed socket would block in `read` until the KEEPER closed its end,
    /// which is exactly the case where nothing else is going to happen.
    stop: Arc<std::sync::atomic::AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl KeeperClient {
    /// Build a client over an already-connected socket.
    ///
    /// The connection path lives in [`crate::client_connect`], and this
    /// constructor is how it hands the result over without widening the
    /// fields' visibility to the whole crate.
    pub(crate) fn establish(
        path: PathBuf,
        stream: UnixStream,
        shared: Arc<Mutex<Shared>>,
        events: Receiver<MuxFrame>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        reader: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            path,
            write_half: Mutex::new(stream),
            shared,
            events,
            deferred: Mutex::new(VecDeque::new()),
            stop,
            reader: Some(reader),
        }
    }
}

impl Drop for KeeperClient {
    /// Close the connection.
    ///
    /// Stopping the reader is the load-bearing part. The reader holds its own
    /// dup of the socket, and a Unix stream is only closed for the peer when
    /// EVERY dup is gone — so a dropped client that left its reader running
    /// would keep the connection open, and the keeper would go on serving a
    /// worker that no longer exists.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl KeeperClient {
    /// merely reached.
    pub fn hello(&self) -> Result<Vec<KeeperFeature>, ClientError> {
        let request = KeeperHelloRequest {
            protocol_version: crate::payloads::KEEPER_PROTOCOL_VERSION,
            requested_features: KeeperFeature::SUPPORTED
                .iter()
                .map(|feature| feature.wire_name().to_string())
                .collect(),
        };
        let reply = self.request(MuxFrameType::Hello, MuxFrameType::HelloResp, 0, &request)?;
        let response: crate::payloads::KeeperHelloResponse = reply
            .parse_json()
            .ok_or_else(|| ClientError::Io("the keeper's hello did not decode".into()))?;
        self.shared
            .lock()
            .expect("the client lock is never held across a wait")
            .keeper = Some(response.observation);

        // A feature the client needs and the keeper lacks makes this keeper
        // unusable, and the reason must name the feature: "it did not work" is
        // what an operator files, and it is not diagnosable.
        for required in KeeperFeature::REQUIRED {
            if !response.features.contains(&required) {
                return Err(ClientError::Unsupported(required.wire_name()));
            }
        }
        Ok(response.features)
    }

    /// The keeper's own observation of itself, from the `Hello` answer.
    pub fn observation(&self) -> Option<KeeperObservation> {
        self.shared.lock().ok()?.keeper.clone()
    }

    /// Open a channel and wait for the acknowledgement.
    ///
    /// The wait is bounded. A keeper that accepts the frame and never answers
    /// is the incident this timeout exists for, so the caller learns about it
    /// rather than hanging.
    pub fn spawn(
        &self,
        channel_id: u16,
        shell_spec: ShellSpec,
        cols: u16,
        rows: u16,
    ) -> Result<u32, ClientError> {
        let request = SpawnRequest {
            channel_id,
            cols,
            rows,
            shell_spec,
        };
        let frame = MuxFrame::json(MuxFrameType::Spawn, channel_id, &request)
            .map_err(|err| ClientError::Io(err.to_string()))?;
        let (sender, receiver) = std::sync::mpsc::channel();
        {
            let mut shared = self
                .shared
                .lock()
                .expect("the client lock is never held across a wait");
            shared.pending.insert(channel_id, PendingSpawn { sender });
        }

        if let Err(err) = self.write(&frame) {
            self.forget(channel_id);
            return Err(err);
        }

        match receiver.recv_timeout(SPAWN_ACK_TIMEOUT) {
            Ok(Ok(pid)) => Ok(pid),
            Ok(Err(err)) => {
                self.forget(channel_id);
                Err(err)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.forget(channel_id);
                Err(ClientError::SpawnNotAcknowledged {
                    path: self.path.clone(),
                    timeout: SPAWN_ACK_TIMEOUT,
                })
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.forget(channel_id);
                Err(ClientError::Io(
                    "the keeper connection closed mid-spawn".into(),
                ))
            }
        }
    }

    /// The channels this keeper still owns, for a worker that is resuming.
    pub fn list_channels(&self) -> Result<ListChannelsResp, ClientError> {
        let reply = self.request(
            MuxFrameType::ListChannels,
            MuxFrameType::ListChannelsResp,
            0,
            &serde_json::json!({}),
        )?;
        reply
            .parse_json()
            .ok_or_else(|| ClientError::Io("the channel list did not decode".into()))
    }

    /// Write input, without waiting for an answer.
    ///
    /// Deliberately not acknowledged: this is on the keystroke path, where a
    /// round trip per character would make the terminal feel broken. The
    /// sequenced form is available for callers that need the guarantee.
    pub fn write_input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), ClientError> {
        let frame = MuxFrame::new(MuxFrameType::PtyIn, channel_id, bytes.to_vec())
            .map_err(|err| ClientError::Io(err.to_string()))?;
        self.write(&frame)
    }

    /// Write input and wait for the keeper to say how much it wrote.
    pub fn write_input_sequenced(
        &self,
        channel_id: u16,
        input_seq: u64,
        bytes: &[u8],
    ) -> Result<crate::payloads::PtyInResult, ClientError> {
        let request = PtyInRequest {
            input_seq,
            bytes: bytes.to_vec(),
        };
        let payload = request.encode();
        let frame = MuxFrame::new(MuxFrameType::PtyInRequest, channel_id, payload.clone())
            .map_err(|err| ClientError::Io(err.to_string()))?;
        self.write(&frame)?;
        let reply = self.wait_for_any_input_result(channel_id, Duration::from_secs(10))?;
        crate::payloads::PtyInResult::decode(reply.frame_type, &reply.payload)
            .ok_or_else(|| ClientError::Io("the input result did not decode".into()))
    }

    /// Ask the keeper to resize a channel, acknowledged.
    pub fn resize(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), ClientError> {
        let payload = crate::payloads::ResizeRequest { seq, cols, rows }
            .encode()
            .map_err(|err| ClientError::Io(err.to_string()))?;
        let tag = self.request_tag(
            MuxFrameType::ResizeRequest,
            MuxFrameType::ResizeAck,
            channel_id,
            &payload,
        )?;
        match tag {
            MuxFrameType::ResizeAck => Ok(()),
            MuxFrameType::ResizeReject => Err(ClientError::Io(format!(
                "the keeper refused the resize of channel {channel_id}"
            ))),
            other => Err(ClientError::Io(format!(
                "unexpected resize reply {other:?}"
            ))),
        }
    }

    /// Hold a frame a control wait consumed, so the worker still receives it.

    pub(crate) fn write(&self, frame: &MuxFrame) -> Result<(), ClientError> {
        let mut socket = self
            .write_half
            .lock()
            .expect("the write lock is never held across a wait");
        socket
            .write_all(&frame.encode())
            .map_err(|err| ClientError::Io(err.to_string()))?;
        socket
            .flush()
            .map_err(|err| ClientError::Io(err.to_string()))
    }

    fn forget(&self, channel_id: u16) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.pending.remove(&channel_id);
        }
    }
}

impl std::fmt::Debug for KeeperClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeeperClient")
            .field("path", &self.path)
            .field("keeper", &self.observation())
            .field(
                "stopped",
                &self.stop.load(std::sync::atomic::Ordering::SeqCst),
            )
            .finish()
    }
}
