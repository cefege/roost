//! The open itself: the one ordered walk from "ask for a device" to a graph that
//! publishes frames.
//!
//! Split out of `super::audio_capture` because the open is the only part of the
//! capture with an order the browser cares about, and the order is the whole
//! content of this file. It builds into locals and publishes to the singleton in
//! one step at the end, because an open outlives its own deadline and a late
//! completion must not overwrite a graph a later tap already built.
//! Ports `openPipeline` of `apps/web/src/voice/audioPcmCapture.ts`.

use js_sys::Reflect;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{AudioContext, MediaStream};

use super::audio_capture::{OPEN_TIMEOUT_MS, RacedFailure, raced, with_mic};
use super::audio_graph::{attach_script_processor, attach_worklet};
use super::capture_facts::CapturePath;
use super::handshake::{
    MicOpenFailure, audio_session_stalled, message_failure, mic_open_failure, mic_open_outcome,
};
use super::pcm::Resampler;

/// The whole open, in the order iOS requires.
pub(super) async fn open_pipeline(generation: u64) -> Result<(), MicOpenFailure> {
    let context = new_context()?;
    // The primer. iOS refuses to start a session that is asked before an input
    // session exists, and answers this rejection itself; the second resume
    // below is the one that has to work.
    let _ = context.resume();
    let stream = open_device().await?;
    if context.state() != web_sys::AudioContextState::Running {
        // The resume answers with the promise it starts; a rejection here is the
        // browser refusing a session it will not grant, which the check below
        // turns into the caption an operator can act on.
        let _ = context
            .resume()
            .map(|promise| promise.catch(&Closure::once(|_error: JsValue| {})));
    }
    if context.state() != web_sys::AudioContextState::Running {
        return Err(message_failure(&audio_session_stalled(state_name(
            context.state(),
        ))));
    }
    let rate = context.sample_rate() as u32;
    let source = context
        .create_media_stream_source(&stream)
        .map_err(|_| mic_open_failure("NotReadableError", "the audio source could not open"))?;
    let attached = if with_mic(|mic| mic.worklet_broken.get()) {
        attach_script_processor(&context, &source, generation);
        false
    } else {
        match attach_worklet(&context, &source, generation).await {
            Ok(()) => true,
            Err(reason) => {
                tracing::warn!(target: "voice", %reason, "audio worklet unavailable");
                with_mic(|mic| mic.worklet_broken.set(true));
                attach_script_processor(&context, &source, generation);
                false
            }
        }
    };
    if with_mic(|mic| mic.generation.get() != generation) {
        // Torn down while opening: the graph this open built is not the one the
        // page wants, and a caller still waiting on it is owed a verdict rather
        // than a device that no longer exists.
        stop_stream(&stream);
        return Err(message_failure("the microphone was released while opening"));
    }
    let path = if attached {
        CapturePath::Worklet
    } else {
        CapturePath::ScriptProcessor
    };
    with_mic(|mic| {
        mic.context = Some(context);
        mic.stream = Some(stream);
        mic.resampler = Some(Resampler::new(rate));
        mic.path.set(path);
    });
    Ok(())
}

/// Construct the browser's `AudioContext`, prefixed where it must be.
fn new_context() -> Result<AudioContext, MicOpenFailure> {
    if let Ok(context) = AudioContext::new() {
        return Ok(context);
    }
    // Safari still prefixes the constructor; the engine is chosen because one of
    // the two names exists, so a refusal here is the browser refusing to build
    // an audio graph at all.
    let constructor = super::capabilities::audio_context_constructor()
        .ok_or_else(|| mic_open_failure("NotReadableError", "this browser has no audio session"))?;
    Reflect::construct(&constructor, &js_sys::Array::new())
        .map_err(|_| mic_open_failure("NotReadableError", "this browser has no audio session"))?
        .dyn_into::<AudioContext>()
        .map_err(|_| mic_open_failure("NotReadableError", "this browser has no audio session"))
}

/// The audio session's own state, in the words the stalled caption uses.
pub fn state_name(state: web_sys::AudioContextState) -> &'static str {
    match state {
        web_sys::AudioContextState::Suspended => "suspended",
        web_sys::AudioContextState::Running => "running",
        web_sys::AudioContextState::Closed => "closed",
        _ => "unknown",
    }
}

/// Ask for the device, with the constraints the recorder needs.
async fn open_device() -> Result<MediaStream, MicOpenFailure> {
    let devices = super::capabilities::media_devices()
        .ok_or_else(|| mic_open_failure("NotFoundError", "this browser exposes no microphone"))?;
    let audio = js_sys::Object::new();
    for (name, value) in [
        ("channelCount", JsValue::from_f64(1.0)),
        ("echoCancellation", JsValue::TRUE),
        ("noiseSuppression", JsValue::TRUE),
    ] {
        let _ = js_sys::Reflect::set(&audio, &JsValue::from_str(name), &value);
    }
    // The audio constraints are set reflectively because the dictionary's own
    // type is not in web-sys, and the browser only reads the three keys.
    let constraints = web_sys::MediaStreamConstraints::new();
    let _ = js_sys::Reflect::set(constraints.as_ref(), &JsValue::from_str("audio"), &audio);
    let promise = devices
        .get_user_media_with_constraints(&constraints)
        .map_err(rejection)?;
    let value = match raced(promise.clone(), OPEN_TIMEOUT_MS, "the microphone").await {
        Ok(value) => value,
        Err(failure @ RacedFailure::Refused(_)) => {
            return Err(mic_open_outcome(&failure.outcome()));
        }
        Err(RacedFailure::Stalled(_)) => {
            // The open is abandoned, not cancelled: a promise that resolves after
            // the deadline still holds a device, and an orphaned track keeps the
            // microphone's indicator lit for the rest of the page's life.
            let orphan = Closure::wrap(Box::new(|stream: JsValue| {
                if stream.is_object() {
                    stop_stream(&super::capabilities::stand_in(stream));
                }
            }) as Box<dyn FnMut(JsValue)>);
            let _ = promise.then(&orphan);
            orphan.forget();
            return Err(mic_open_outcome(
                &RacedFailure::Stalled("the microphone").outcome(),
            ));
        }
    };
    Ok(super::capabilities::stand_in(value))
}

/// Read a rejected `getUserMedia` into the operator's vocabulary.
fn rejection(error: JsValue) -> MicOpenFailure {
    let name = js_sys::Reflect::get(&error, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    let message = js_sys::Reflect::get(&error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    mic_open_failure(&name, &message)
}

/// Release a device. Every track is a stand-in-tolerant read: a stream this
/// crate did not get from `getUserMedia` still has to be let go.
pub(super) fn stop_stream(stream: &MediaStream) {
    for track in stream.get_tracks().iter() {
        if track.is_object() {
            super::capabilities::stand_in::<web_sys::MediaStreamTrack>(track).stop();
        }
    }
}
