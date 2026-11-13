//! Real microphone capture and incoming playback after canonical authentication.
//! Called only by live_audio/connect; prepare never creates a media element.
//! Unclaimed late device grants are drained and every live track has an owner.

use futures::{
    channel::oneshot,
    future::{AbortHandle, Abortable},
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Event, HtmlAudioElement, MediaStream, MediaStreamConstraints, MediaStreamTrack,
    RtcPeerConnection, RtcPeerConnectionState, RtcSdpType, RtcSessionDescriptionInit,
    RtcTrackEvent,
};

use super::{
    live_audio::{LiveAudioState, fail_later},
    live_peer::{LivePeer, stop_stream},
    live_wait,
};

pub(super) async fn connect(
    peer: Rc<LivePeer>,
    owner: Weak<RefCell<LiveAudioState>>,
    answer: String,
) -> Result<Option<String>, String> {
    let stream = open_microphone().await?;
    if peer.closed.get() {
        stop_stream(&stream);
        return Err("Live audio was closed while opening the microphone.".into());
    }
    *peer.local_stream.borrow_mut() = Some(stream.clone());
    let tracks = stream.get_audio_tracks();
    if tracks.length() != 1 {
        return Err("The browser did not provide one microphone audio track.".into());
    }
    let track = tracks
        .get(0)
        .dyn_into::<MediaStreamTrack>()
        .map_err(|_| "The browser returned an invalid microphone track.")?;
    track.set_enabled(!peer.muted.get());
    watch_track(
        &peer,
        owner,
        &track,
        "The microphone was disconnected or stopped.",
    );
    live_wait::promise(
        peer.transceiver.sender().replace_track(Some(&track)),
        "Attaching the microphone",
    )
    .await?;
    let audio = HtmlAudioElement::new()
        .map_err(|error| live_wait::refusal("Creating live audio playback", error))?;
    audio
        .set_attribute("playsinline", "")
        .map_err(|error| live_wait::refusal("Preparing live audio playback", error))?;
    *peer.audio.borrow_mut() = Some(audio);
    let (sender, ready) = oneshot::channel();
    *peer.media_ready.borrow_mut() = Some(sender);
    let description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    description.set_sdp(&answer);
    live_wait::promise(
        peer.connection.set_remote_description(&description),
        "Applying live audio signaling",
    )
    .await
    .map_err(|_| "The browser rejected the live audio SDP answer.".to_owned())?;
    wait_connected(&peer.connection).await?;
    live_wait::bounded(
        async move {
            ready
                .await
                .map_err(|_| "Live audio playback was cancelled.".to_owned())?
        },
        "Starting incoming live audio",
    )
    .await?;
    tracing::info!(target: "agent_chat", peer_id = %peer.peer_id, "OMP live audio connected");
    Ok(None)
}

async fn open_microphone() -> Result<MediaStream, String> {
    let window = web_sys::window().ok_or("Live audio requires a browser window.")?;
    let devices = window
        .navigator()
        .media_devices()
        .map_err(|error| live_wait::refusal("Accessing the microphone", error))?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&wasm_bindgen::JsValue::TRUE);
    constraints.set_video(&wasm_bindgen::JsValue::FALSE);
    let promise = devices
        .get_user_media_with_constraints(&constraints)
        .map_err(|error| live_wait::refusal("Opening the microphone", error))?;
    let (sender, receiver) = oneshot::channel();
    // The device promise cannot be cancelled. This drain owns no view or peer;
    // dropping the receiver makes a late grant stop its tracks immediately.
    spawn_local(async move {
        let result = JsFuture::from(promise)
            .await
            .map_err(|error| live_wait::refusal("Opening the microphone", error))
            .and_then(|value| {
                value
                    .dyn_into::<MediaStream>()
                    .map_err(|_| "The browser returned an invalid microphone stream.".into())
            });
        if let Err(Ok(stream)) = sender.send(result) {
            stop_stream(&stream);
        }
    });
    live_wait::bounded(
        async move {
            receiver
                .await
                .map_err(|_| "Opening the microphone was cancelled.".to_owned())?
        },
        "Opening the microphone",
    )
    .await
}

pub(super) fn bind_incoming(peer: &Rc<LivePeer>, owner: Weak<RefCell<LiveAudioState>>) {
    let weak = Rc::downgrade(peer);
    let callback = Closure::wrap(Box::new(move |event: Event| {
        let Some(peer) = weak.upgrade().filter(|peer| !peer.closed.get()) else {
            return;
        };
        let Ok(event) = event.dyn_into::<RtcTrackEvent>() else {
            return;
        };
        let track = event.track();
        if track.kind() != "audio"
            || !peer.connecting.get()
            || peer.remote_stream.borrow().is_some()
        {
            track.stop();
            fail_later(
                owner.clone(),
                Rc::downgrade(&peer),
                "The live peer returned unexpected media.".into(),
            );
            return;
        }
        let stream = match MediaStream::new() {
            Ok(stream) => stream,
            Err(error) => {
                track.stop();
                fail_later(
                    owner.clone(),
                    Rc::downgrade(&peer),
                    live_wait::refusal("Receiving live audio", error),
                );
                return;
            }
        };
        stream.add_track(&track);
        *peer.remote_stream.borrow_mut() = Some(stream.clone());
        watch_track(&peer, owner.clone(), &track, "Incoming live audio ended.");
        let Some(audio) = peer.audio.borrow().as_ref().cloned() else {
            fail_later(
                owner.clone(),
                Rc::downgrade(&peer),
                "Live audio playback was not prepared.".into(),
            );
            return;
        };
        audio.set_src_object(Some(&stream));
        let playback = match audio.play() {
            Ok(playback) => playback,
            Err(error) => {
                fail_later(
                    owner.clone(),
                    Rc::downgrade(&peer),
                    live_wait::refusal("Playing incoming live audio", error),
                );
                return;
            }
        };
        let playback_owner = owner.clone();
        let (abort, registration) = AbortHandle::new_pair();
        peer.tasks.borrow_mut().push(abort);
        spawn_local(async move {
            let result = Abortable::new(
                live_wait::promise(playback, "Playing incoming live audio"),
                registration,
            )
            .await;
            if let Ok(result) = result {
                if let Some(sender) = peer.media_ready.borrow_mut().take() {
                    let _ = sender.send(result.as_ref().map(|_| ()).map_err(Clone::clone));
                }
                if let Err(error) = result {
                    fail_later(playback_owner, Rc::downgrade(&peer), error);
                }
            }
        });
    }) as Box<dyn FnMut(Event)>);
    peer.connection
        .set_ontrack(Some(callback.as_ref().unchecked_ref()));
    peer.callbacks.borrow_mut().push(callback);
}

fn watch_track(
    peer: &Rc<LivePeer>,
    owner: Weak<RefCell<LiveAudioState>>,
    track: &MediaStreamTrack,
    message: &'static str,
) {
    let weak = Rc::downgrade(peer);
    let callback = Closure::wrap(Box::new(move |_: Event| {
        fail_later(owner.clone(), weak.clone(), message.into());
    }) as Box<dyn FnMut(Event)>);
    track.set_onended(Some(callback.as_ref().unchecked_ref()));
    peer.track_callbacks
        .borrow_mut()
        .push((track.clone(), callback));
}

struct ConnectionWatch {
    connection: RtcPeerConnection,
    callback: Closure<dyn FnMut(Event)>,
}

impl Drop for ConnectionWatch {
    fn drop(&mut self) {
        let _ = self.connection.remove_event_listener_with_callback(
            "connectionstatechange",
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

async fn wait_connected(connection: &RtcPeerConnection) -> Result<(), String> {
    if connection.connection_state() == RtcPeerConnectionState::Connected {
        return Ok(());
    }
    let (sender, receiver) = oneshot::channel();
    let mut sender = Some(sender);
    let watched = connection.clone();
    let callback = Closure::wrap(Box::new(move |_: Event| {
        let result = match watched.connection_state() {
            RtcPeerConnectionState::Connected => Ok(()),
            RtcPeerConnectionState::Failed
            | RtcPeerConnectionState::Disconnected
            | RtcPeerConnectionState::Closed => Err("The live audio connection failed.".to_owned()),
            _ => return,
        };
        if let Some(sender) = sender.take() {
            let _ = sender.send(result);
        }
    }) as Box<dyn FnMut(Event)>);
    connection
        .add_event_listener_with_callback(
            "connectionstatechange",
            callback.as_ref().unchecked_ref(),
        )
        .map_err(|error| live_wait::refusal("Watching the live connection", error))?;
    let _watch = ConnectionWatch {
        connection: connection.clone(),
        callback,
    };
    if connection.connection_state() == RtcPeerConnectionState::Connected {
        return Ok(());
    }
    live_wait::bounded(
        async move {
            receiver
                .await
                .map_err(|_| "The live audio connection was cancelled.".to_owned())?
        },
        "Connecting live audio",
    )
    .await
}
