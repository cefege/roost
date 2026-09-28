//! The [`NativePeer`] a connection holds for one str0m peer: the answer
//! (gather, then accept the offer, then start the driver), sends that
//! libdatachannel would have accepted or buffered, and close. Returned by
//! `peer::native::factory`; called by the terminal and attachment peer
//! connections. Ports the node-datachannel `PeerConnection`/`DataChannel`
//! methods v2 `apps/worker/src/terminal/peer/terminal-peer-connection.ts` calls.

use std::sync::Arc;
use std::time::Duration;

use str0m::change::SdpOffer;
use str0m::{Candidate, Rtc};

use super::gather::gather;
use super::str0m_peer::{PeerIo, PeerShared};
use super::{NativeFingerprint, NativePeer, NativePeerError, driver};
use crate::uplink::OwnerFuture;

#[derive(Debug)]
pub(super) struct Str0mPeer {
    shared: Arc<PeerShared>,
}

impl Str0mPeer {
    pub(super) fn new(shared: Arc<PeerShared>) -> Self {
        Self { shared }
    }
}

impl NativePeer for Str0mPeer {
    fn answer(
        &self,
        offer_sdp: String,
        gathering_deadline: Duration,
    ) -> OwnerFuture<Result<String, NativePeerError>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            let offer =
                SdpOffer::from_sdp_string(&offer_sdp).map_err(|_| NativePeerError::InvalidOffer)?;
            {
                let mut io = shared.lock();
                if io.closed {
                    return Err(NativePeerError::Closed);
                }
                if io.answered {
                    return Err(NativePeerError::AlreadyAnswered);
                }
                io.answered = true;
            }
            let deadline = tokio::time::Instant::now() + gathering_deadline;
            let gathered = gather(&shared.config, deadline).await?;
            let answer = {
                let mut io = shared.lock();
                if io.closed {
                    return Err(NativePeerError::Closed);
                }
                for candidate in gathered.candidates {
                    add_local_candidate(&mut io.rtc, candidate);
                }
                io.rtc
                    .sdp_api()
                    .accept_offer(offer)
                    .map_err(|_| NativePeerError::InvalidOffer)?
                    .to_sdp_string()
            };
            driver::spawn(Arc::clone(&shared), gathered.sockets);
            tracing::debug!(peer = %shared.config.name, "a native peer answered its offer");
            Ok(answer)
        })
    }

    fn remote_fingerprint(&self) -> Option<NativeFingerprint> {
        self.shared.lock().remote_fingerprint.clone()
    }

    fn send(&self, channel: usize, bytes: &[u8]) -> Result<bool, NativePeerError> {
        if bytes.len() > self.shared.config.max_message_size {
            return Err(NativePeerError::MessageTooLarge(bytes.len()));
        }
        let mut io = self.shared.lock();
        if io.closed {
            return Err(NativePeerError::Closed);
        }
        let PeerIo { rtc, channels, .. } = &mut *io;
        let slot = channels
            .get_mut(channel)
            .ok_or(NativePeerError::UnknownChannel(channel))?;
        if !slot.open || slot.closed {
            return Err(NativePeerError::Closed);
        }
        // Behind earlier buffered bytes a message waits its turn, so the
        // channel stays ordered.
        let sent_now = match (slot.pending.is_empty(), rtc.channel(slot.id)) {
            (true, Some(mut writer)) => writer
                .write(true, bytes)
                .map_err(|error| NativePeerError::Transport(error.to_string()))?,
            _ => false,
        };
        if !sent_now {
            slot.pending.push_back(bytes.to_vec());
            slot.pending_bytes += bytes.len();
            if slot.pending_bytes > slot.low_threshold {
                slot.above_threshold = true;
            }
        }
        drop(io);
        self.shared.wake.notify_one();
        Ok(sent_now)
    }

    fn buffered_amount(&self, channel: usize) -> usize {
        self.shared
            .lock()
            .channels
            .get(channel)
            .map_or(0, |slot| slot.pending_bytes)
    }

    fn set_buffered_amount_low_threshold(&self, channel: usize, bytes: usize) {
        if let Some(slot) = self.shared.lock().channels.get_mut(channel) {
            slot.low_threshold = bytes;
        }
    }

    fn is_open(&self, channel: usize) -> bool {
        let io = self.shared.lock();
        !io.closed
            && io
                .channels
                .get(channel)
                .is_some_and(|slot| slot.open && !slot.closed)
    }

    fn close_channel(&self, channel: usize) {
        let mut io = self.shared.lock();
        let PeerIo { rtc, channels, .. } = &mut *io;
        if let Some(slot) = channels.get(channel) {
            if !slot.closed && rtc.is_alive() {
                rtc.direct_api().close_data_channel(slot.id);
            }
        }
        drop(io);
        self.shared.wake.notify_one();
    }

    fn close(&self) {
        let mut io = self.shared.lock();
        if io.closed {
            return;
        }
        io.closed = true;
        io.rtc.disconnect();
        drop(io);
        tracing::debug!(peer = %self.shared.config.name, "a native peer was closed by its owner");
        self.shared.wake.notify_one();
    }
}

fn add_local_candidate(rtc: &mut Rtc, candidate: Candidate) {
    if rtc.add_local_candidate(candidate).is_none() {
        tracing::debug!("str0m refused a gathered local candidate");
    }
}
