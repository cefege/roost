//! How the composer and the mic share one draft.
//!
//! Split out of `super::composer` so the composer's own body reads as layout and
//! submission, and every rule about what a recording does to the draft — the
//! base it started from, the provisional tail it may not touch — sits in one
//! place. Ports `apps/web/src/components/terminal/TerminalComposeDictation.ts`.

use std::cell::RefCell;

use dioxus::prelude::*;

use crate::components::mobile_voice_send::FinalizeSendDoor;
use crate::voice::send_gate::SendGate;
use crate::voice::state::{LiveTranscript, VoiceState};
use crate::voice::transcript::{PaintedDraft, glued, paint};

/// The composer's side of one recording.
pub(super) struct DictationBinding {
    /// A Dioxus signal handle needs `&mut` to write and the binding is shared by
    /// every callback the mic raises, so each written signal sits behind its own
    /// borrow.
    draft: RefCell<Signal<String>>,
    dictation_base: RefCell<Signal<Option<String>>>,
    provisional_from: RefCell<Signal<Option<usize>>>,
    voice_state: RefCell<Signal<VoiceState>>,
    /// Where a Send pressed while the recording finalizes reaches the mic.
    send_door: FinalizeSendDoor,
}

/// Wire the composer's draft to the mic.
pub(super) fn use_dictation(draft: Signal<String>) -> DictationBinding {
    DictationBinding {
        draft: RefCell::new(draft),
        dictation_base: RefCell::new(use_signal(|| Option::<String>::None)),
        provisional_from: RefCell::new(use_signal(|| Option::<usize>::None)),
        voice_state: RefCell::new(use_signal(VoiceState::default)),
        send_door: use_hook(FinalizeSendDoor::default),
    }
}

impl DictationBinding {
    fn draft(&self) -> String {
        let value = self.draft.borrow();
        value.read().to_owned()
    }

    /// An interim update, painted onto the draft the recording started from.
    pub(super) fn show(&self, update: Option<LiveTranscript>) {
        let Some(update) = update else {
            return;
        };
        let base = match self.dictation_base.borrow().read().clone() {
            Some(base) => base,
            None => self.draft(),
        };
        self.dictation_base.borrow_mut().set(Some(base.clone()));
        let painted = paint(&base, &update);
        self.draft.borrow_mut().set(painted.text);
        self.provisional_from
            .borrow_mut()
            .set(painted.provisional_from);
    }

    /// The recording ended with words to insert.
    pub(super) fn commit(&self, text: String) {
        let base = self
            .dictation_base
            .borrow_mut()
            .take()
            .unwrap_or_else(|| self.draft());
        self.provisional_from.borrow_mut().set(None);
        self.draft.borrow_mut().set(glued(&base, &text));
    }

    /// The operator threw the recording away.
    pub(super) fn discard(&self, _unit: ()) {
        self.provisional_from.borrow_mut().set(None);
        if let Some(base) = self.dictation_base.borrow_mut().take() {
            self.draft.borrow_mut().set(base);
        }
    }

    /// Where the mic's recording is, as it last reported.
    pub(super) fn set_voice_state(&self, state: VoiceState) {
        self.voice_state.borrow_mut().set(state);
    }

    /// A keystroke ends the provisional paint: what the operator typed is now the
    /// draft, and the words the recognizer guessed are not.
    pub(super) fn forget_provisional(&self) {
        self.dictation_base.borrow_mut().set(None);
        self.provisional_from.borrow_mut().set(None);
    }

    /// The placeholder: a recording owns the field, and says so.
    pub(super) fn placeholder(&self) -> &'static str {
        if self.voice_state.borrow().read().is_dictating() {
            "Listening…"
        } else {
            "Type terminal input…"
        }
    }

    /// What a Send press does while the mic is where it is.
    pub(super) fn send_gate(&self) -> SendGate {
        SendGate::for_state(*self.voice_state.borrow().read())
    }

    /// Queue a Send with the mic, which submits the draft once the stopped
    /// recording's last words are in it.
    pub(super) fn send_after_finalize(&self) {
        self.send_door.press();
    }

    /// The ghost mirror's split of the current field text.
    pub(super) fn ghost(&self) -> PaintedDraft {
        let value = self.draft();
        match *self.provisional_from.borrow().read() {
            Some(from) if from <= value.len() => PaintedDraft {
                text: value,
                provisional_from: Some(from),
            },
            _ => PaintedDraft {
                text: value,
                provisional_from: None,
            },
        }
    }

    /// The draft as it may be STORED.
    ///
    /// The field shows the whole paint — an unproven hypothesis and all — so
    /// that a wrong guess can be dropped without having touched what a send
    /// would use. A draft that outlives this composer does not get that
    /// privilege: it returns from a store as ordinary text, with no mirror and
    /// nothing left to drop it, so what is written is the draft without the
    /// tail the recognizer guessed.
    pub(super) fn persisted(&self) -> String {
        self.ghost().persisted()
    }
}

/// A comparable handle on a recording's binding, for a component prop: a prop
/// must be comparable and a bundle of signals is not.
#[derive(Clone, Debug)]
pub struct SharedBinding(pub(super) DictationBinding);

impl std::fmt::Debug for DictationBinding {
    /// A binding is a bundle of signal handles; what identifies it is that it is
    /// THE binding, so it prints as its own name rather than as its handles.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DictationBinding")
    }
}

impl Clone for DictationBinding {
    fn clone(&self) -> Self {
        Self {
            draft: self.draft.clone(),
            dictation_base: self.dictation_base.clone(),
            provisional_from: self.provisional_from.clone(),
            voice_state: self.voice_state.clone(),
            send_door: self.send_door.clone(),
        }
    }
}

impl PartialEq for SharedBinding {
    /// Two handles are the same handle when they name the same signals, which is
    /// what a re-render of this prop needs to know.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(
            self.0.dictation_base.as_ptr(),
            other.0.dictation_base.as_ptr(),
        )
    }
}

/// The composer's keyterm context: the screen the renderer owns, plus the words
/// the operator has spelled here.
pub(super) fn use_dictation_context(
    screen: Option<crate::voice::keyterms::ContextReader>,
    draft: Signal<String>,
) -> crate::voice::keyterms::ContextReader {
    crate::voice::keyterms::ContextReader::new(move || {
        let seen = screen
            .as_ref()
            .map_or_else(crate::voice::keyterms::TerminalContext::default, |reader| {
                reader.call()
            });
        crate::voice::keyterms::TerminalContext {
            input: (draft)(),
            ..seen
        }
    })
}

/// The mirror the field paints over itself while a hypothesis is unproven.
#[component]
pub fn GhostMirror(ghost: PaintedDraft) -> Element {
    rsx! {
        div {
            class: "term-chat__ghost",
            "data-testid": "chat-ghost",
            aria_hidden: "true",
            span { "{ghost.settled_head()}" }
            span {
                class: "term-chat__ghost-tail",
                "data-testid": "chat-ghost-tail",
                "{ghost.provisional_tail()}"
            }
        }
    }
}

/// The mic, wired to the composer's draft.
#[component]
pub fn VoiceControl(
    owner_id: String,
    #[props(default)] active: bool,
    #[props(default)] read_context: Option<crate::voice::keyterms::ContextReader>,
    binding: SharedBinding,
    /// The draft is ready to submit: Send was pressed while the recording
    /// finalized, and its words are in the draft now.
    on_submit: EventHandler<()>,
) -> Element {
    let send_door = binding.0.send_door.clone();
    let commit_binding = binding.0.clone();
    let live_binding = binding.0.clone();
    let discard_binding = binding.0.clone();
    rsx! {
        crate::components::mobile_voice_input::MobileVoiceInput {
            owner_id,
            active,
            on_state_change: move |state: VoiceState| binding.0.set_voice_state(state),
            on_transcript: move |text: String| commit_binding.commit(text),
            on_live_transcript: move |update: Option<LiveTranscript>| live_binding.show(update),
            on_discard: move |()| discard_binding.discard(()),
            on_submit: move |()| on_submit.call(()),
            send_door: Some(send_door),
            read_context,
        }
    }
}
