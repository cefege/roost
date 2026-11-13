//! Browser WebRTC resources for one source-issued live peer UUID.
//! Host prepare creates signaling resources only; authenticated connect owns media.
//! Every callback, operation and track is detached or stopped before retirement.

use futures::{channel::oneshot, future::AbortHandle};
use roost_client_core::client::agent_chat::OmpHostService;
use roost_protocol::wire::agent_chat::MAX_OMP_LIVE_AUDIO_BYTES;
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{
    Event, HtmlAudioElement, MediaStream, MediaStreamTrack, RtcDataChannel, RtcIceGatheringState,
    RtcPeerConnection, RtcPeerConnectionState, RtcRtpTransceiver, RtcRtpTransceiverDirection,
    RtcRtpTransceiverInit, RtcSessionDescriptionInit,
};

use super::{
    controller::OmpHostServiceConfig,
    live_audio::{LiveAudioState, fail_later},
    live_wait,
};

type EventCallback = Closure<dyn FnMut(Event)>;

pub(super) struct LivePeer {
    pub(super) peer_id: String,
    pub(super) config: OmpHostServiceConfig,
    pub(super) connection: RtcPeerConnection,
    pub(super) transceiver: RtcRtpTransceiver,
    pub(super) channel: RtcDataChannel,
    pub(super) local_stream: RefCell<Option<MediaStream>>,
    pub(super) remote_stream: RefCell<Option<MediaStream>>,
    pub(super) audio: RefCell<Option<HtmlAudioElement>>,
    pub(super) callbacks: RefCell<Vec<EventCallback>>,
    pub(super) track_callbacks: RefCell<Vec<(MediaStreamTrack, EventCallback)>>,
    pub(super) tasks: RefCell<Vec<AbortHandle>>,
    pub(super) active_service: RefCell<Option<OmpHostService>>,
    pub(super) card_target: RefCell<Option<(String, String)>>,
    pub(super) media_ready: RefCell<Option<oneshot::Sender<Result<(), String>>>>,
    pub(super) offer: RefCell<Option<String>>,
    pub(super) connecting: Cell<bool>,
    pub(super) muted: Cell<bool>,
    pub(super) closed: Cell<bool>,
}

impl LivePeer {
    pub(super) fn new(
        peer_id: String,
        config: OmpHostServiceConfig,
        owner: Weak<RefCell<LiveAudioState>>,
    ) -> Result<Rc<Self>, String> {
        let connection = RtcPeerConnection::new()
            .map_err(|error| live_wait::refusal("Creating the live connection", error))?;
        let init = RtcRtpTransceiverInit::new();
        init.set_direction(RtcRtpTransceiverDirection::Sendrecv);
        let transceiver = connection.add_transceiver_with_str_and_init("audio", &init);
        let channel = connection.create_data_channel("oai-events");
        let peer = Rc::new(Self {
            peer_id,
            config,
            connection,
            transceiver,
            channel,
            local_stream: RefCell::new(None),
            remote_stream: RefCell::new(None),
            audio: RefCell::new(None),
            callbacks: RefCell::new(Vec::new()),
            track_callbacks: RefCell::new(Vec::new()),
            tasks: RefCell::new(Vec::new()),
            media_ready: RefCell::new(None),
            active_service: RefCell::new(None),
            card_target: RefCell::new(None),
            offer: RefCell::new(None),
            connecting: Cell::new(false),
            muted: Cell::new(false),
            closed: Cell::new(false),
        });
        peer.bind_callbacks(owner);
        tracing::info!(target: "agent_chat", peer_id = %peer.peer_id, "OMP live audio prepared signaling resources");
        Ok(peer)
    }

    fn bind_callbacks(self: &Rc<Self>, owner: Weak<RefCell<LiveAudioState>>) {
        let weak = Rc::downgrade(self);
        let connection_owner = owner.clone();
        let connection = Closure::wrap(Box::new(move |_: Event| {
            let Some(peer) = weak.upgrade().filter(|peer| !peer.closed.get()) else {
                return;
            };
            if matches!(
                peer.connection.connection_state(),
                RtcPeerConnectionState::Failed
                    | RtcPeerConnectionState::Disconnected
                    | RtcPeerConnectionState::Closed
            ) {
                fail_later(
                    connection_owner.clone(),
                    Rc::downgrade(&peer),
                    "The live audio connection was lost.".into(),
                );
            }
        }) as Box<dyn FnMut(Event)>);
        self.connection
            .set_onconnectionstatechange(Some(connection.as_ref().unchecked_ref()));
        self.callbacks.borrow_mut().push(connection);
        for is_error in [false, true] {
            let weak = Rc::downgrade(self);
            let channel_owner = owner.clone();
            let callback = Closure::wrap(Box::new(move |_: Event| {
                let Some(peer) = weak.upgrade().filter(|peer| !peer.closed.get()) else {
                    return;
                };
                fail_later(
                    channel_owner.clone(),
                    Rc::downgrade(&peer),
                    "The live audio event channel closed or failed.".into(),
                );
            }) as Box<dyn FnMut(Event)>);
            if is_error {
                self.channel
                    .set_onerror(Some(callback.as_ref().unchecked_ref()));
            } else {
                self.channel
                    .set_onclose(Some(callback.as_ref().unchecked_ref()));
            }
            self.callbacks.borrow_mut().push(callback);
        }
        super::live_media::bind_incoming(self, owner);
    }

    pub(super) async fn prepare(self: &Rc<Self>) -> Result<Option<String>, String> {
        if let Some(offer) = self.offer.borrow().as_ref() {
            return Ok(Some(offer.clone()));
        }
        let offer = live_wait::promise(
            self.connection.create_offer(),
            "Creating the live audio offer",
        )
        .await?;
        let offer: RtcSessionDescriptionInit = offer.unchecked_into();
        live_wait::promise(
            self.connection.set_local_description(&offer),
            "Preparing live audio signaling",
        )
        .await
        .map_err(|_| "The browser rejected the live audio SDP offer.".to_owned())?;
        self.gather_ice().await?;
        let sdp = self
            .connection
            .local_description()
            .map(|description| description.sdp())
            .filter(|sdp| !sdp.is_empty() && sdp.len() <= MAX_OMP_LIVE_AUDIO_BYTES)
            .ok_or("The browser produced an empty or oversized live audio offer.")?;
        *self.offer.borrow_mut() = Some(sdp.clone());
        Ok(Some(sdp))
    }

    async fn gather_ice(&self) -> Result<(), String> {
        if self.connection.ice_gathering_state() == RtcIceGatheringState::Complete {
            return Ok(());
        }
        let (sender, receiver) = oneshot::channel();
        let mut sender = Some(sender);
        let connection = self.connection.clone();
        let callback = Closure::wrap(Box::new(move |_: Event| {
            if connection.ice_gathering_state() == RtcIceGatheringState::Complete
                && let Some(sender) = sender.take()
            {
                let _ = sender.send(());
            }
        }) as Box<dyn FnMut(Event)>);
        self.connection
            .set_onicegatheringstatechange(Some(callback.as_ref().unchecked_ref()));
        let _gathering = IceGathering {
            connection: self.connection.clone(),
            _callback: callback,
        };
        if self.connection.ice_gathering_state() == RtcIceGatheringState::Complete {
            return Ok(());
        }
        live_wait::bounded(
            async move {
                receiver
                    .await
                    .map_err(|_| "Live audio signaling was cancelled.".into())
            },
            "Gathering live audio network candidates",
        )
        .await
    }

    pub(super) fn mute(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(stream) = self.local_stream.borrow().as_ref() {
            for track in stream
                .get_audio_tracks()
                .iter()
                .filter_map(|track| track.dyn_into::<MediaStreamTrack>().ok())
            {
                track.set_enabled(!muted);
            }
        }
        tracing::info!(target: "agent_chat", peer_id = %self.peer_id, muted, "OMP live audio mute changed");
    }

    pub(super) fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.connection.set_onconnectionstatechange(None);
        self.connection.set_ontrack(None);
        self.connection.set_onicegatheringstatechange(None);
        self.channel.set_onerror(None);
        self.channel.set_onclose(None);
        for (track, _) in self.track_callbacks.borrow().iter() {
            track.set_onended(None);
        }
        for task in self.tasks.borrow_mut().drain(..) {
            task.abort();
        }
        if let Some(audio) = self.audio.borrow_mut().take() {
            let _ = audio.pause();
            audio.set_src_object(None);
            audio.remove();
        }
        if let Some(stream) = self.local_stream.borrow_mut().take() {
            stop_stream(&stream);
        }
        if let Some(stream) = self.remote_stream.borrow_mut().take() {
            stop_stream(&stream);
        }
        self.channel.close();
        self.connection.close();
        self.callbacks.borrow_mut().clear();
        self.track_callbacks.borrow_mut().clear();
        self.active_service.borrow_mut().take();
        self.media_ready.borrow_mut().take();
        self.offer.borrow_mut().take();
        tracing::info!(target: "agent_chat", peer_id = %self.peer_id, "OMP live audio closed");
    }
}

impl Drop for LivePeer {
    fn drop(&mut self) {
        self.close();
    }
}

struct IceGathering {
    connection: RtcPeerConnection,
    _callback: EventCallback,
}

impl Drop for IceGathering {
    fn drop(&mut self) {
        self.connection.set_onicegatheringstatechange(None);
    }
}

pub(super) fn stop_stream(stream: &MediaStream) {
    for track in stream
        .get_tracks()
        .iter()
        .filter_map(|track| track.dyn_into::<MediaStreamTrack>().ok())
    {
        track.stop();
    }
}
