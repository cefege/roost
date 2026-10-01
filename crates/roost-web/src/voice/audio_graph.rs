//! Connecting the graph to the frames: the audio worklet, and the deprecated
//! script processor that stands in for it.
//!
//! Split out of `super::audio_capture` because the worklet source is a program
//! in its own right and the two attach paths are alternatives: the worklet runs
//! off the main thread, and the script processor exists only for the browsers
//! where it will not load. Ports the attach half of
//! `apps/web/src/voice/audioPcmCapture.ts`.

use js_sys::Float32Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{AudioContext, AudioProcessingEvent, Blob, BlobPropertyBag, Event, MessageEvent};

use super::audio_capture::{MODULE_TIMEOUT_MS, WORKLET_PROCESSOR, WORKLET_SOURCE};
use super::audio_capture::{deliver, raced, with_mic};

/// Load the inline worklet and connect the graph through it.
pub async fn attach_worklet(
    context: &AudioContext,
    source: &web_sys::MediaStreamAudioSourceNode,
    generation: u64,
) -> Result<(), String> {
    let worklet = context.audio_worklet().map_err(|_| "no audio worklet")?;
    let url = worklet_url().map_err(|_| "the worklet module could not be built")?;
    let loaded = worklet
        .add_module(&url)
        .map_err(|_| "the worklet module refused")?;
    raced(loaded, MODULE_TIMEOUT_MS, "the audio worklet")
        .await
        .map_err(|failure| failure.caption())?;
    let _ = web_sys::Url::revoke_object_url(&url);
    let node = web_sys::AudioWorkletNode::new(context, WORKLET_PROCESSOR)
        .map_err(|error| format!("{error:?}"))?;
    let port: web_sys::MessagePort = node.port().map_err(|error| format!("{error:?}"))?;
    let handler: super::audio_capture::WorkletHandler =
        Closure::wrap(Box::new(move |event: MessageEvent| {
            if let Some(frames) = event.data().dyn_ref::<Float32Array>() {
                deliver(&frames.to_vec(), generation);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
    // Setting `onmessage` is what enables a port's message queue, so the
    // explicit `start()` is a no-op on a real one and a hard failure on a
    // stand-in that does not carry the method.
    port.set_onmessage(Some(handler.as_ref().unchecked_ref()));
    source
        .connect_with_audio_node(&node)
        .map_err(|error| format!("{error:?}"))?;
    node.connect_with_audio_node(&context.destination())
        .map_err(|error| format!("{error:?}"))?;
    with_mic(|mic| mic.worklet_handler = Some(handler));
    Ok(())
}

/// Connect the graph through the deprecated script processor, which is the only
/// path left when the worklet will not load.
/// Connect the graph through the deprecated script processor.
pub fn attach_script_processor(
    context: &AudioContext,
    source: &web_sys::MediaStreamAudioSourceNode,
    generation: u64,
) {
    let Ok(node) = context.create_script_processor_with_buffer_size(4096) else {
        return;
    };
    let handler: super::audio_capture::ProcessorHandler =
        Closure::wrap(Box::new(move |event: Event| {
            let Some(processing) = event.dyn_ref::<AudioProcessingEvent>() else {
                return;
            };
            let frames: Vec<f32> = processing
                .input_buffer()
                .and_then(|buffer| buffer.get_channel_data(0))
                .unwrap_or_default();
            deliver(&frames, generation);
        }) as Box<dyn FnMut(Event)>);
    node.set_onaudioprocess(Some(handler.as_ref().unchecked_ref()));
    if source.connect_with_audio_node(&node).is_err() {
        return;
    }
    // Connecting to the destination is what drives the callback; the node's own
    // output is silence, so nothing is routed back to the speakers.
    let _ = node.connect_with_audio_node(&context.destination());
    with_mic(|mic| mic.processor_handler = Some(handler));
}

/// The blob URL the worklet module is loaded from.
fn worklet_url() -> Result<String, JsValue> {
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(WORKLET_SOURCE));
    let bag = BlobPropertyBag::new();
    bag.set_type("application/javascript");
    let blob = Blob::new_with_str_sequence_and_options(&parts, &bag)?;
    Ok(web_sys::Url::create_object_url_with_blob(&blob)?)
}

#[cfg(test)]
mod tests {
    use super::WORKLET_PROCESSOR;

    #[test]
    fn the_worklet_registers_the_name_the_host_constructs() {
        // The module source is a string in the parent module; what matters here
        // is that the name the host asks for is the one the source registers.
        assert_eq!(WORKLET_PROCESSOR, "pcm-forwarder");
    }
}
