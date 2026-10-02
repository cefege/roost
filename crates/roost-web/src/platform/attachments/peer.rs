//! The attachment WebRTC carrier: a fresh `RTCPeerConnection` per upload with
//! the two pre-negotiated attachment channels, never shared with the terminal
//! peer.
//!
//! Opened by `upload_host` when no matching door carried the upload. The SDP
//! rules are `client::attachments::signaling`'s and the frame and packet rules
//! `client::attachments::peer::AttachmentPeerTransfer`'s; this owns the browser
//! objects, their callbacks, and the deadlines. Ports `attachmentPeer.ts` and
//! `attachmentPeerSignaling.ts` in `apps/web/src/client/attachments/`.

use js_sys::Reflect;
use roost_client_core::client::attachments::conversation::ConversationOutcome;
use roost_client_core::client::attachments::direct::loopback::STATUS_DEADLINE_MS;
use roost_client_core::client::attachments::grant::AttachmentDirectGrant;
use roost_client_core::client::attachments::packets::{
    ATTACHMENT_PEER_DATA_CHANNEL_PROTOCOL, ATTACHMENT_PEER_LANES, PeerLane,
};
use roost_client_core::client::attachments::peer::{
    ACK_DEADLINE_MS, AttachmentPeerTransfer, HELLO_DEADLINE_MS, PeerDeadline,
};
use roost_client_core::client::attachments::signaling::{
    AttachmentPeerNegotiationRequest, AttachmentPeerNegotiationResponse, AttachmentPeerSignaling,
    ICE_GATHERING_DEADLINE_MS, NEGOTIATION_DEADLINE_MS,
};
use roost_client_core::client::attachments::transfer::receipt::AttachmentTransferStatus;
use roost_client_core::client::attachments::transfer::{
    AttachmentTransferAck, AttachmentTransferCarrierError, InFlightChunk,
};
use wasm_bindgen::JsCast as _;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    RtcDataChannel, RtcDataChannelInit, RtcDataChannelState, RtcPeerConnection, RtcSdpType,
    RtcSessionDescriptionInit,
};

use super::AttachmentCarrier;
use super::inbox::{Deadline, EventInbox};
use super::peer_objects::{
    PeerEvent, PeerListeners, construct, install_channel_listeners, install_connection_listeners,
    string_prop,
};

/// An authenticated attachment peer for one upload.
#[derive(Debug)]
pub struct AttachmentPeerCarrier {
    connection: RtcPeerConnection,
    channels: Vec<(PeerLane, RtcDataChannel)>,
    inbox: EventInbox<PeerEvent>,
    transfer: AttachmentPeerTransfer,
    released: bool,
    _listeners: PeerListeners,
}

impl AttachmentPeerCarrier {
    /// Build the peer, negotiate it through the coordinator, and wait for the
    /// worker to authenticate it. Every failure is a refusal before any chunk.
    pub async fn open(
        grant: &AttachmentDirectGrant,
        peer_id: &str,
        negotiate: impl AsyncFn(
            AttachmentPeerNegotiationRequest,
        ) -> Result<AttachmentPeerNegotiationResponse, String>,
    ) -> Result<Self, AttachmentTransferCarrierError> {
        let mut transfer = AttachmentPeerTransfer::new(grant.clone(), peer_id);
        let Some(connection) = construct(&grant.stun_urls) else {
            return Err(transfer.finish("attachment peer could not open"));
        };
        let inbox = EventInbox::new();
        let mut listeners = install_connection_listeners(&connection, &inbox);
        let mut channels = Vec::new();
        for definition in ATTACHMENT_PEER_LANES {
            let init = RtcDataChannelInit::new();
            init.set_negotiated(true);
            init.set_id(definition.id);
            init.set_ordered(true);
            init.set_protocol(ATTACHMENT_PEER_DATA_CHANNEL_PROTOCOL);
            let channel =
                connection.create_data_channel_with_data_channel_dict(definition.label, &init);
            let _ = Reflect::set(&channel, &"binaryType".into(), &"arraybuffer".into());
            listeners.extend(install_channel_listeners(&channel, definition.lane, &inbox));
            channels.push((definition.lane, channel));
        }
        let mut carrier = Self {
            connection,
            channels,
            inbox,
            transfer,
            released: false,
            _listeners: listeners,
        };
        if let Err(reason) = carrier.negotiate(grant, peer_id, negotiate).await {
            tracing::info!(target: "attachments", %reason, "attachment peer negotiation failed");
            let refusal = carrier.transfer.finish("attachment peer setup failed");
            carrier.release();
            return Err(refusal);
        }
        match carrier.await_authentication().await {
            Ok(()) => {
                tracing::info!(target: "attachments", peer = %peer_id, "attachment peer carrier ready");
                Ok(carrier)
            }
            Err(refusal) => {
                carrier.release();
                Err(refusal)
            }
        }
    }

    /// Offer, gather, exchange through the coordinator, apply the answer.
    async fn negotiate(
        &mut self,
        grant: &AttachmentDirectGrant,
        peer_id: &str,
        negotiate: impl AsyncFn(
            AttachmentPeerNegotiationRequest,
        ) -> Result<AttachmentPeerNegotiationResponse, String>,
    ) -> Result<(), String> {
        let mut signaling = AttachmentPeerSignaling::new(grant.stun_urls.clone());
        signaling
            .begin_gathering()
            .map_err(|error| error.to_string())?;
        let offer = JsFuture::from(self.connection.create_offer())
            .await
            .map_err(|_| "createOffer failed".to_owned())?;
        JsFuture::from(
            self.connection
                .set_local_description(&offer.unchecked_into()),
        )
        .await
        .map_err(|_| "setLocalDescription failed".to_owned())?;
        if string_prop(&self.connection, "iceGatheringState").as_deref() != Some("complete") {
            // v2 offers whatever was gathered when the deadline lapses rather
            // than failing: a host candidate is often all a loopback peer needs.
            let deadline = Deadline::after(ICE_GATHERING_DEADLINE_MS);
            loop {
                match self.inbox.next(&deadline).await {
                    None | Some(PeerEvent::GatheringComplete) => break,
                    Some(PeerEvent::Ended(reason)) => return Err(reason.to_owned()),
                    Some(_) => {}
                }
            }
        }
        let local_sdp = self
            .connection
            .local_description()
            .map(|description| description.sdp())
            .unwrap_or_default();
        let offer_sdp = signaling
            .settle_offer(&local_sdp)
            .map_err(|error| error.to_string())?;
        let request = signaling.negotiation_request(grant, peer_id, &offer_sdp);
        let response = negotiate(request).await?;
        let answer_sdp = signaling
            .admit_answer(peer_id, grant, &response)
            .map_err(|error| error.to_string())?;
        let answer = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        answer.set_sdp(&answer_sdp);
        JsFuture::from(self.connection.set_remote_description(&answer))
            .await
            .map_err(|_| "setRemoteDescription failed".to_owned())?;
        signaling.finish().map_err(|error| error.to_string())
    }

    /// Wait for both channels to open and the worker's `Ready`. The hello
    /// deadline starts when the control channel opens, as v2's does; before
    /// that only the negotiation deadline bounds the wait.
    async fn await_authentication(&mut self) -> Result<(), AttachmentTransferCarrierError> {
        let mut deadline = Deadline::after(NEGOTIATION_DEADLINE_MS);
        loop {
            match self.inbox.next(&deadline).await {
                None => return Err(self.elapsed(PeerDeadline::Hello)),
                Some(PeerEvent::ChannelOpen(lane)) => {
                    if lane == PeerLane::Control {
                        deadline = Deadline::after(HELLO_DEADLINE_MS);
                    }
                    self.transfer.channel_opened(lane)?;
                    self.flush()?;
                }
                Some(PeerEvent::Packet(lane, bytes)) => {
                    self.transfer.packet_received(lane, &bytes, now_ms())?;
                }
                Some(PeerEvent::BufferedLow) => self.flush()?,
                Some(PeerEvent::GatheringComplete) => {}
                Some(PeerEvent::Ended(reason)) => return Err(self.transfer.finish(reason)),
            }
            if self.transfer.is_authenticated() {
                return Ok(());
            }
        }
    }

    /// Wait for the frame that settles the outstanding chunk or receipt.
    async fn await_settlement(
        &mut self,
        deadline_ms: u64,
        deadline_kind: PeerDeadline,
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        let deadline = Deadline::after(deadline_ms);
        loop {
            let failure = match self.inbox.next(&deadline).await {
                // Ambiguous and NOT closed: the receipt that settles a lost
                // acknowledgement is asked for on this same peer next.
                None => return Err(self.elapsed(deadline_kind)),
                Some(PeerEvent::Packet(lane, bytes)) => {
                    match self.transfer.packet_received(lane, &bytes, now_ms()) {
                        Ok(
                            outcome
                            @ (ConversationOutcome::Ack(_) | ConversationOutcome::Status(_)),
                        ) => {
                            return Ok(outcome);
                        }
                        Ok(_) => continue,
                        Err(error) => error,
                    }
                }
                Some(PeerEvent::BufferedLow) => match self.flush() {
                    Ok(()) => continue,
                    Err(error) => error,
                },
                Some(PeerEvent::ChannelOpen(_) | PeerEvent::GatheringComplete) => continue,
                Some(PeerEvent::Ended(reason)) => self.transfer.finish(reason),
            };
            self.release();
            return Err(failure);
        }
    }

    /// Put every queued fragment on its open channel.
    fn flush(&mut self) -> Result<(), AttachmentTransferCarrierError> {
        loop {
            let open: Vec<PeerLane> = self
                .channels
                .iter()
                .filter(|(_, channel)| channel.ready_state() == RtcDataChannelState::Open)
                .map(|(lane, _)| *lane)
                .collect();
            let packets = self.transfer.flush(&open)?;
            if packets.is_empty() {
                return Ok(());
            }
            for packet in packets {
                let sent = self
                    .channels
                    .iter()
                    .find(|(lane, _)| *lane == packet.lane)
                    .is_some_and(|(_, channel)| channel.send_with_u8_array(&packet.bytes).is_ok());
                if !sent {
                    return Err(self
                        .transfer
                        .finish("attachment peer data channel send failed"));
                }
            }
        }
    }

    fn elapsed(&mut self, deadline: PeerDeadline) -> AttachmentTransferCarrierError {
        match self.transfer.deadline_elapsed(deadline) {
            Err(error) => error,
            Ok(()) => self.transfer.finish("attachment peer deadline elapsed"),
        }
    }

    /// Unhook every callback and close the channels and the connection.
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        for (_, channel) in &self.channels {
            channel.set_onopen(None);
            channel.set_onclose(None);
            channel.set_onerror(None);
            channel.set_onbufferedamountlow(None);
            channel.set_onmessage(None);
            channel.close();
        }
        self.connection.set_ondatachannel(None);
        self.connection.set_ontrack(None);
        self.connection.set_onconnectionstatechange(None);
        self.connection.set_oniceconnectionstatechange(None);
        self.connection.set_onicegatheringstatechange(None);
        self.connection.close();
    }
}

impl AttachmentCarrier for AttachmentPeerCarrier {
    async fn send_chunk(
        &mut self,
        chunk: &InFlightChunk,
        data: Vec<u8>,
    ) -> Result<AttachmentTransferAck, AttachmentTransferCarrierError> {
        self.transfer.send_chunk(chunk, data)?;
        if let Err(error) = self.flush() {
            self.release();
            return Err(error);
        }
        match self
            .await_settlement(ACK_DEADLINE_MS, PeerDeadline::Ack)
            .await?
        {
            ConversationOutcome::Ack(ack) => Ok(ack),
            _ => Err(self
                .transfer
                .finish("attachment peer received an invalid frame")),
        }
    }

    async fn request_status(
        &mut self,
        upload_id: &str,
    ) -> Result<AttachmentTransferStatus, AttachmentTransferCarrierError> {
        self.transfer.request_status(upload_id)?;
        if let Err(error) = self.flush() {
            self.release();
            return Err(error);
        }
        match self
            .await_settlement(STATUS_DEADLINE_MS, PeerDeadline::Status)
            .await?
        {
            ConversationOutcome::Status(status) => Ok(*status),
            _ => Err(self
                .transfer
                .finish("attachment peer returned invalid status")),
        }
    }

    fn sent_chunk(&self) -> bool {
        self.transfer.sent_chunk()
    }

    fn close(&mut self, reason: &str) {
        let _ = self.transfer.finish(reason);
        self.release();
    }
}

impl Drop for AttachmentPeerCarrier {
    fn drop(&mut self) {
        self.release();
    }
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}
