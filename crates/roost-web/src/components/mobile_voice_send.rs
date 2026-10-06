//! How a Send pressed in the composer reaches the mic while it finalizes.
//!
//! The composer renders Send and the mic owns the recording, so a press made
//! while a stopped recording waits for its last words has to cross from parent
//! to child: the composer holds a door, and the mounted mic installs itself in
//! it. Called by `super::mobile_voice_input` and the terminal composer.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use dioxus::prelude::*;

use super::mobile_voice_input::ComposerContext;
use crate::voice::state::VoiceEvent;

/// The composer's handle on the mic it mounts.
///
/// Weak, because the mic's lifetime is its own: once it unmounts a press reaches
/// nothing, and the recording it would have waited on has already ended through
/// `ComposerContext::finish`.
#[derive(Clone, Default)]
pub struct FinalizeSendDoor(Rc<RefCell<Weak<ComposerContext>>>);

impl FinalizeSendDoor {
    /// Queue a Send with the mounted mic, which submits the draft once the
    /// words its stopped recording waits for have landed in it.
    pub fn press(&self) {
        let context = self.0.borrow().upgrade();
        if let Some(context) = context {
            tracing::debug!(target: "voice", "voice.send_queued");
            context.apply(VoiceEvent::SendPressed);
        }
    }
}

impl PartialEq for FinalizeSendDoor {
    /// One composer's door, whichever render handed down this clone of it.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for FinalizeSendDoor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FinalizeSendDoor")
    }
}

/// Install `context` in the owning composer's door when this mic mounts.
pub(super) fn use_finalize_send_door(
    door: Option<FinalizeSendDoor>,
    context: &Rc<ComposerContext>,
) {
    use_hook(|| {
        if let Some(door) = door {
            *door.0.borrow_mut() = Rc::downgrade(context);
        }
    });
}
