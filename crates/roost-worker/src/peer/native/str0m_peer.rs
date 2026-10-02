//! One str0m `Rtc` and the state node-datachannel kept beside it: each
//! channel's open flag, the accepted bytes SCTP had no room for yet, and the
//! low-water bookkeeping; plus the turn that feeds str0m input and turns its
//! output into the callbacks v2 relied on. Driven by `peer::native::driver`;
//! reached through `peer::native::peer_handle`. Ports the node-datachannel half
//! of v2 `apps/worker/src/terminal/peer/terminal-peer-native.ts`.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Event, IceConnectionState, Input, Output, Rtc};
use tokio::sync::{Notify, mpsc};

use super::{NativeFingerprint, NativePeerConfig, NativePeerEvent, NativePeerEvents};

/// How long a closed peer's last step asks the driver to wait; it exits first.
const CLOSED_STEP: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) struct ChannelSlot {
    pub(super) id: ChannelId,
    pub(super) open: bool,
    pub(super) closed: bool,
    /// Accepted bytes SCTP had no room for yet — libdatachannel's buffer.
    pub(super) pending: VecDeque<Vec<u8>>,
    pub(super) pending_bytes: usize,
    pub(super) low_threshold: usize,
    pub(super) above_threshold: bool,
}

impl ChannelSlot {
    pub(super) fn new(id: ChannelId) -> Self {
        Self {
            id,
            open: false,
            closed: false,
            pending: VecDeque::new(),
            pending_bytes: 0,
            low_threshold: 0,
            above_threshold: false,
        }
    }
}

pub(super) struct PeerIo {
    pub(super) rtc: Rtc,
    pub(super) channels: Vec<ChannelSlot>,
    pub(super) answered: bool,
    pub(super) closed: bool,
    /// Closed by its owner while connected: the driver's next turn sends what
    /// is queued (the channels' stream resets), then disconnects.
    pub(super) closing: bool,
    pub(super) remote_fingerprint: Option<NativeFingerprint>,
}

impl std::fmt::Debug for PeerIo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PeerIo")
            .field("channels", &self.channels.len())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

/// What one driver turn produced: datagrams to send, when to turn again, and
/// whether the peer is still worth driving.
#[derive(Debug)]
pub(super) struct DriveStep {
    pub(super) transmits: Vec<Transmit>,
    pub(super) deadline: Instant,
    pub(super) alive: bool,
}

/// A datagram one of the peer's sockets received.
#[derive(Debug)]
pub(super) struct Datagram {
    pub(super) source: SocketAddr,
    pub(super) destination: SocketAddr,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct PeerShared {
    pub(super) config: NativePeerConfig,
    io: Mutex<PeerIo>,
    /// Rings the driver after a send or close so its output is polled now.
    pub(super) wake: Notify,
    events: mpsc::UnboundedSender<NativePeerEvent>,
}

impl PeerShared {
    pub(super) fn new(
        config: NativePeerConfig,
        rtc: Rtc,
        channels: Vec<ChannelSlot>,
    ) -> (Arc<Self>, NativePeerEvents) {
        let (events, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Self {
            config,
            io: Mutex::new(PeerIo {
                rtc,
                channels,
                answered: false,
                closed: false,
                closing: false,
                remote_fingerprint: None,
            }),
            wake: Notify::new(),
            events,
        });
        (shared, receiver)
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, PeerIo> {
        self.io.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: NativePeerEvent) {
        // A connection that stopped listening has retired this peer already.
        let _ = self.events.send(event);
    }

    /// One str0m turn: feed the input, drain every output, move buffered
    /// sends into SCTP, and report the channels that fell below their mark.
    pub(super) fn drive(&self, datagram: Option<&Datagram>, now: Instant) -> DriveStep {
        let mut io = self.lock();
        let mut step = DriveStep {
            transmits: Vec::new(),
            deadline: now + CLOSED_STEP,
            alive: false,
        };
        if io.closed {
            return step;
        }
        let fed = match datagram {
            Some(datagram) => match datagram.bytes.as_slice().try_into() {
                Ok(contents) => io.rtc.handle_input(Input::Receive(
                    now,
                    Receive {
                        proto: Protocol::Udp,
                        source: datagram.source,
                        destination: datagram.destination,
                        contents,
                    },
                )),
                // Not a STUN/DTLS/RTP datagram: nothing a peer can act on.
                Err(_) => Ok(()),
            },
            None => io.rtc.handle_input(Input::Timeout(now)),
        };
        if let Err(error) = fed {
            self.fail(&mut io, &error.to_string());
            return step;
        }
        while self.poll_until_timeout(&mut io, &mut step) && self.flush_pending(&mut io) {}
        self.report_low_water(&mut io);
        if io.closing && !io.closed {
            io.closed = true;
            io.rtc.disconnect();
            tracing::debug!(peer = %self.config.name, transmits = step.transmits.len(), "a closing native peer sent its last datagrams");
        }
        if !io.closed && !io.rtc.is_alive() {
            io.closed = true;
            tracing::info!(peer = %self.config.name, "a native peer closed underneath its owner");
            self.emit(NativePeerEvent::Closed);
        }
        step.alive = !io.closed;
        step
    }

    /// False once the peer failed.
    fn poll_until_timeout(&self, io: &mut PeerIo, step: &mut DriveStep) -> bool {
        loop {
            match io.rtc.poll_output() {
                Ok(Output::Transmit(transmit)) => step.transmits.push(transmit),
                Ok(Output::Event(event)) => self.on_event(io, event),
                Ok(Output::Timeout(deadline)) => {
                    step.deadline = deadline;
                    return !io.closed;
                }
                Err(error) => {
                    self.fail(io, &error.to_string());
                    return false;
                }
            }
        }
    }

    fn on_event(&self, io: &mut PeerIo, event: Event) {
        match event {
            Event::Connected => {
                io.remote_fingerprint =
                    io.rtc.direct_api().remote_dtls_fingerprint().map(|remote| {
                        let hex: Vec<String> = remote
                            .bytes
                            .iter()
                            .map(|byte| format!("{byte:02X}"))
                            .collect();
                        NativeFingerprint {
                            algorithm: remote.hash_func.clone(),
                            value: hex.join(":"),
                        }
                    });
                tracing::debug!(peer = %self.config.name, "a native peer connected");
                self.emit(NativePeerEvent::Connected);
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                tracing::debug!(peer = %self.config.name, "a native peer lost ICE connectivity");
                self.emit(NativePeerEvent::Failed);
            }
            Event::ChannelOpen(id, _) => match slot_index(io, id) {
                Some(index) => {
                    io.channels[index].open = true;
                    self.emit(NativePeerEvent::ChannelOpen(index));
                }
                None => self.emit(NativePeerEvent::UnsolicitedChannel),
            },
            Event::ChannelData(data) => {
                if let Some(channel) = slot_index(io, data.id) {
                    self.emit(NativePeerEvent::ChannelMessage {
                        channel,
                        binary: data.binary,
                        data: data.data,
                    });
                }
            }
            Event::ChannelClose(id) => {
                if let Some(index) = slot_index(io, id) {
                    let slot = &mut io.channels[index];
                    slot.open = false;
                    slot.closed = true;
                    slot.pending.clear();
                    slot.pending_bytes = 0;
                    self.emit(NativePeerEvent::ChannelClosed(index));
                }
            }
            Event::MediaAdded(_) => self.emit(NativePeerEvent::UnsolicitedChannel),
            _ => {}
        }
    }

    /// Moves buffered sends into SCTP in channel order, so the first channel
    /// keeps its priority. True when anything moved, which leaves output to poll.
    fn flush_pending(&self, io: &mut PeerIo) -> bool {
        let PeerIo { rtc, channels, .. } = io;
        let mut moved = false;
        let mut failed = Vec::new();
        for (index, slot) in channels.iter_mut().enumerate() {
            while let Some(bytes) = slot.pending.front() {
                let length = bytes.len();
                let Some(mut channel) = rtc.channel(slot.id) else {
                    break;
                };
                match channel.write(true, bytes) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(_) => {
                        failed.push(index);
                        break;
                    }
                }
                slot.pending_bytes -= length;
                slot.pending.pop_front();
                moved = true;
            }
        }
        for index in failed {
            tracing::debug!(peer = %self.config.name, channel = index, "a buffered channel write failed");
            self.emit(NativePeerEvent::ChannelError(index));
        }
        moved
    }

    fn report_low_water(&self, io: &mut PeerIo) {
        for (index, slot) in io.channels.iter_mut().enumerate() {
            if slot.above_threshold && slot.pending_bytes <= slot.low_threshold {
                slot.above_threshold = false;
                self.emit(NativePeerEvent::BufferedAmountLow(index));
            }
        }
    }

    fn fail(&self, io: &mut PeerIo, reason: &str) {
        if io.closed {
            return;
        }
        io.closed = true;
        io.rtc.disconnect();
        tracing::info!(peer = %self.config.name, reason, "a native peer failed");
        self.emit(NativePeerEvent::Failed);
    }

    /// The driver lost its sockets: the peer cannot carry anything more.
    pub(super) fn fail_transport(&self, reason: &str) {
        let mut io = self.lock();
        self.fail(&mut io, reason);
    }
}

fn slot_index(io: &PeerIo, id: ChannelId) -> Option<usize> {
    io.channels.iter().position(|slot| slot.id == id)
}
