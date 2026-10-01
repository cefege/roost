//! The one composer a shell action can drive the microphone through.
//!
//! A pad binding and a keyboard shortcut reach dictation from outside the DOM,
//! and only the mounted composer owns the recording — so it publishes its
//! commands here and the page keeps one, exactly as `voiceControls()` did in
//! `apps/web/src/voice/voiceState.ts`. The registry is a `thread_local` because
//! the reader is a poll callback with no component to re-render.
//!
//! Identity is the claim's token, so a replacement composer can register before
//! the previous instance releases, and the stale release cannot clear the
//! winner's commands.

use std::cell::RefCell;
use std::rc::Rc;

/// What one mounted composer offers a shell action.
#[derive(Clone)]
pub struct VoiceControls {
    /// Start, stop-and-send, or commit — the mic's own activation.
    pub toggle: Rc<dyn Fn()>,
    /// Throw the owned recording away.
    pub discard: Rc<dyn Fn()>,
    /// Whether a recording is open for this composer right now.
    pub dictating: Rc<dyn Fn() -> bool>,
    /// Whether a start would be honoured without a fresh user gesture.
    pub can_start_without_gesture: Rc<dyn Fn() -> bool>,
}

impl std::fmt::Debug for VoiceControls {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VoiceControls")
            .finish_non_exhaustive()
    }
}

thread_local! {
    /// The mounted composer's commands, or nobody.
    static SLOT: RefCell<Option<(u64, VoiceControls)>> = const { RefCell::new(None) };
    /// The next token to hand out. Starts at 1 so `0` stays reserved for nobody.
    static NEXT_TOKEN: RefCell<u64> = const { RefCell::new(1) };
}

/// A registration that stands down when its composer unmounts.
///
/// Dropping it clears the slot only while it still owns it, so a composer that
/// replaced a live one does not take the live one's commands with it.
#[derive(Debug)]
pub struct VoiceControlClaim {
    token: u64,
}

impl Drop for VoiceControlClaim {
    fn drop(&mut self) {
        SLOT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(token, _)| *token == self.token) {
                *slot = None;
            }
        });
        tracing::debug!(target: "voice", token = self.token, "voice controls released");
    }
}

/// Publish `controls` as the page's dictation commands.
pub fn register(controls: VoiceControls) -> Rc<VoiceControlClaim> {
    let token = NEXT_TOKEN.with(|next| {
        let mut next = next.borrow_mut();
        *next += 1;
        *next
    });
    SLOT.with(|slot| *slot.borrow_mut() = Some((token, controls)));
    tracing::debug!(target: "voice", token, "voice controls registered");
    Rc::new(VoiceControlClaim { token })
}

/// What the controller router reads about the mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DictationFacts {
    /// A recording is owned somewhere on the page.
    pub dictating: bool,
    /// A composer is mounted and answering shell commands.
    pub controls_mounted: bool,
    /// A start would be honoured without a fresh user gesture.
    pub can_start_without_gesture: bool,
}

/// The page's dictation facts, read by a shell action.
pub fn dictation_facts() -> DictationFacts {
    SLOT.with(|slot| match slot.borrow().as_ref() {
        Some((_, controls)) => DictationFacts {
            dictating: (controls.dictating)(),
            controls_mounted: true,
            can_start_without_gesture: (controls.can_start_without_gesture)(),
        },
        None => DictationFacts::default(),
    })
}

/// Run the mounted composer's activation. A no-op with none mounted.
pub fn toggle() {
    let commands = SLOT.with(|slot| slot.borrow().as_ref().map(|(_, controls)| controls.clone()));
    if let Some(controls) = commands {
        (controls.toggle)();
    }
}

/// Throw the owned recording away. A no-op with none mounted.
pub fn discard() {
    let commands = SLOT.with(|slot| slot.borrow().as_ref().map(|(_, controls)| controls.clone()));
    if let Some(controls) = commands {
        (controls.discard)();
    }
}
/// The harness runs each case on its own thread and the slot is a
/// `thread_local`, so a case starts with no composer mounted.
#[cfg(test)]
mod tests {
    use super::{DictationFacts, VoiceControls, dictation_facts, discard, register, toggle};
    use std::cell::Cell;
    use std::rc::Rc;

    fn controls(
        toggles: Rc<Cell<u32>>,
        discards: Rc<Cell<u32>>,
        dictating: Rc<Cell<bool>>,
    ) -> VoiceControls {
        VoiceControls {
            toggle: Rc::new(move || toggles.set(toggles.get() + 1)),
            discard: Rc::new(move || discards.set(discards.get() + 1)),
            dictating: Rc::new(move || dictating.get()),
            can_start_without_gesture: Rc::new(|| true),
        }
    }

    #[test]
    fn an_unmounted_page_has_no_dictation_and_refuses_the_commands() {
        assert_eq!(dictation_facts(), DictationFacts::default());
        toggle();
        discard();
        assert!(!dictation_facts().controls_mounted);
    }

    #[test]
    fn a_registered_composer_answers_toggle_and_discard() {
        let toggles = Rc::new(Cell::new(0));
        let discards = Rc::new(Cell::new(0));
        let dictating = Rc::new(Cell::new(false));
        let _claim = register(controls(
            Rc::clone(&toggles),
            Rc::clone(&discards),
            Rc::clone(&dictating),
        ));
        assert_eq!(
            dictation_facts(),
            DictationFacts {
                dictating: false,
                controls_mounted: true,
                can_start_without_gesture: true,
            }
        );
        toggle();
        dictating.set(true);
        discard();
        assert_eq!(toggles.get(), 1);
        assert_eq!(discards.get(), 1);
        assert!(dictation_facts().dictating);
    }

    #[test]
    fn an_unmounted_composer_takes_its_commands_with_it() {
        let toggles = Rc::new(Cell::new(0));
        {
            let _claim = register(controls(
                Rc::clone(&toggles),
                Rc::new(Cell::new(0)),
                Rc::new(Cell::new(false)),
            ));
            assert!(dictation_facts().controls_mounted);
        }
        assert!(!dictation_facts().controls_mounted);
        toggle();
        assert_eq!(toggles.get(), 0, "an unmounted composer must not be driven");
    }
}
