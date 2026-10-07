//! When the warm microphone hands the device back.
//!
//! A graph kept open between recordings keeps the browser's recording indicator
//! lit, so an idle graph is released `idle_release_ms` after its last recording
//! and at once when the page is hidden. Neither release touches a recording: a
//! graph with a sink attached, or an open still in flight, is not idle.
//! Called by `super::audio_capture`; ports `armIdle` / `releaseMicIfIdle` of
//! `apps/web/src/voice/audioPcmCapture.ts`.

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::audio_capture::{release_mic, with_mic};
use super::capture_facts::{idle_release_ms, is_warm};

/// A callback the page holds for as long as it lives.
pub(super) type ReleaseHandler = Closure<dyn FnMut()>;

/// Put an idle graph on the release clock, restarting any clock already armed.
/// A graph that is delivering to a recording is never on it.
pub(super) fn arm_idle_release() {
    clear_idle_release();
    let idle = with_mic(|mic| mic.sink.is_none());
    if !idle || !is_warm() {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let delay = idle_release_ms();
    let timer = with_mic(|mic| {
        let handler = mic
            .idle_handler
            .get_or_insert_with(|| Closure::new(release_mic_if_idle));
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                handler.as_ref().unchecked_ref(),
                delay,
            )
            .ok()
    });
    with_mic(|mic| mic.idle_timer = timer);
}

/// Stop the release clock: a recording took the graph, or it is already gone.
pub(super) fn clear_idle_release() {
    let timer = with_mic(|mic| mic.idle_timer.take());
    if let (Some(timer), Some(window)) = (timer, web_sys::window()) {
        window.clear_timeout_with_handle(timer);
    }
}

/// Release the page's microphone when the page is hidden, once per page.
pub(super) fn watch_page_visibility() {
    if with_mic(|mic| mic.visibility_handler.is_some()) {
        return;
    }
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let handler: ReleaseHandler = Closure::new(|| {
        let hidden = web_sys::window()
            .and_then(|window| window.document())
            .is_some_and(|document| document.hidden());
        if hidden {
            release_mic_if_idle();
        }
    });
    let _ = document
        .add_event_listener_with_callback("visibilitychange", handler.as_ref().unchecked_ref());
    with_mic(|mic| mic.visibility_handler = Some(handler));
}

/// The automatic release. A recording that is still running — even one in a
/// hidden tab on a phone that switched apps — keeps its device.
fn release_mic_if_idle() {
    let idle = with_mic(|mic| {
        mic.idle_timer = None;
        mic.sink.is_none() && !mic.open.pending()
    });
    if !idle || !is_warm() {
        return;
    }
    tracing::info!(target: "voice", "voice.mic_released_idle");
    release_mic();
}
