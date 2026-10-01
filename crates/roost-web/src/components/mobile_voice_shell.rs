//! How a command from outside the DOM reaches the mounted mic.
//!
//! The composer's own tap is a DOM event; a controller's mic button, a keyboard
//! shortcut or any other shell action is not, and only the mounted composer owns
//! the recording. This publishes that composer's commands in the page's voice
//! slot for as long as it is mounted, so `voice::shell_controls` has one owner
//! to ask rather than a search of the document.

use std::rc::Rc;

use dioxus::prelude::*;

use super::mobile_voice_input::ComposerContext;
use crate::voice::shell_controls::{self, VoiceControls};

/// Publish `context`'s dictation commands and hand back its activation.
///
/// The returned closure is what the mic button runs, so a tap and a shell
/// action are the same call and cannot drift apart.
pub(super) fn use_voice_shell_controls(context: Rc<ComposerContext>, active: bool) -> Rc<dyn Fn()> {
    let toggle: Rc<dyn Fn()> = Rc::new({
        let context = Rc::clone(&context);
        move || context.toggle_recording(active)
    });
    let registered = Rc::clone(&toggle);
    use_hook(move || {
        shell_controls::register(VoiceControls {
            toggle: Rc::clone(&registered),
            discard: Rc::new({
                let context = Rc::clone(&context);
                move || context.discard()
            }),
            dictating: Rc::new({
                let context = Rc::clone(&context);
                move || context.machine().state().is_dictating()
            }),
            can_start_without_gesture: Rc::new(mic_startable),
        })
    });
    toggle
}

/// Whether a dictation may start from a shell action rather than a tap.
///
/// The browser only honours a microphone request inside a gesture, so a
/// permission-queried grant or a still-warm device is what makes a
/// controller-driven start legal.
fn mic_startable() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::voice::capabilities::mic_permission().can_start_without_gesture()
    }
    #[cfg(not(target_arch = "wasm32"))]
    false
}
