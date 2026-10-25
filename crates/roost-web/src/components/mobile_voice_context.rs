//! The state one mic shares with its callbacks: the machine, caption, engine and
//! the effects the machine asks for. Owned by `mobile_voice_input`, which
//! renders it; the watchdog and the shell controls reach it through `apply`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
use super::mobile_voice_dom::warm_mic_for;
use super::mobile_voice_dom::{EngineHosts, is_secure_context};
use super::mobile_voice_watchdog::{arm_watchdog, clear_watchdog};
use crate::pump::Pump;
#[cfg(target_arch = "wasm32")]
use crate::voice::deepgram_engine::{EngineEvent, EngineSink};
use crate::voice::engine::{Engine, start_refusal};
use crate::voice::keyterms::ContextReader;
use crate::voice::ownership::{VoiceSlot, with_slot};
use crate::voice::state::{LiveTranscript, VoiceEffect, VoiceEvent, VoiceMachine, VoiceState};

/// Everything one mic's callbacks need, so a timer and a tap perform the same
/// work through one door.
pub(super) struct ComposerContext {
    /// A Dioxus signal handle needs `&mut` to write, and this struct is shared
    /// by every callback a recording spawns, so the two written signals sit
    /// behind their own borrows.
    pub(super) machine: RefCell<Signal<VoiceMachine>>,
    pub(super) error: RefCell<Signal<Option<String>>>,
    pub(super) engine: Signal<Engine>,
    pub(super) language: Signal<String>,
    pub(super) hosts: Rc<RefCell<EngineHosts>>,
    pub(super) pump: Pump,
    pub(super) session_id: String,
    pub(super) token: u64,
    pub(super) on_state_change: EventHandler<VoiceState>,
    pub(super) on_transcript: EventHandler<String>,
    pub(super) on_live_transcript: EventHandler<Option<LiveTranscript>>,
    pub(super) on_discard: EventHandler<()>,
    pub(super) on_submit: EventHandler<()>,
    pub(super) read_context: Option<ContextReader>,
}

impl ComposerContext {
    /// The machine as it stands, for a reader outside this module.
    pub(super) fn machine(&self) -> VoiceMachine {
        self.machine.borrow().read().clone()
    }

    /// Open the device on the press, so the tap that follows records against a
    /// warm mic. iOS grants no microphone outside a gesture, and the press is
    /// the gesture.
    fn warm_mic(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            if (self.engine)() == Engine::Deepgram {
                crate::voice::warm_mic();
            }
            crate::voice::probe_mic_permission();
        }
        #[cfg(not(target_arch = "wasm32"))]
        warm_mic_for((self.engine)());
    }

    /// Run one event through the machine and perform what it returns.
    pub(super) fn apply(self: &Rc<Self>, event: VoiceEvent) {
        let effects = self.machine.borrow_mut().write().apply(event);
        self.perform(&effects);
    }

    /// Throw the recording away.
    pub(super) fn discard(self: &Rc<Self>) {
        self.apply(VoiceEvent::Discard);
    }

    /// The recording is over because this composer is going away.
    ///
    /// The same door `VoiceEvent::Deactivated` opens for a pane that still
    /// exists: what settled is committed, the hypothesis that was never spoken
    /// is dropped, the engine is aborted and the claim released. A composer
    /// that never got as far as recording still claimed the slot before its
    /// start was refused, and an idle machine answers with no effect at all, so
    /// that claim is handed back here — otherwise the composer that mounts next
    /// carries a different token, and a slot only its holder may release
    /// leaves the page's microphone unstartable for good.
    pub(super) fn finish(self: &Rc<Self>) {
        clear_watchdog();
        let recording = self.machine().state().is_dictating();
        self.apply(VoiceEvent::Deactivated);
        if !recording {
            with_slot(|slot: &mut VoiceSlot| slot.release(self.token));
        }
    }

    /// The mic's own activation, whether a tap or a shell action asked for it.
    pub(super) fn toggle_recording(self: &Rc<Self>, active: bool) {
        let claimed = with_slot(|slot: &mut VoiceSlot| slot.claim(&self.session_id, self.token));
        // A refusal is decided before the machine is asked, because the caption
        // names the page's own problem and not the recording's.
        if let Some(refusal) = start_refusal((self.engine)(), is_secure_context()) {
            self.error.borrow_mut().set(Some(refusal.to_owned()));
            self.machine.borrow_mut().set(VoiceMachine::default());
            return;
        }
        // A stopping tap must not reopen a released device and keep it lit.
        if self.machine().state() == VoiceState::Idle {
            self.warm_mic();
        }
        self.apply(VoiceEvent::Toggle {
            active,
            claimed,
            engine_available: (self.engine)().is_available(),
        });
    }

    fn perform(self: &Rc<Self>, effects: &[VoiceEffect]) {
        for effect in effects {
            match effect {
                VoiceEffect::ClaimVoice => {
                    with_slot(|slot: &mut VoiceSlot| {
                        slot.claim(&self.session_id, self.token);
                    });
                }
                VoiceEffect::ReleaseVoice => {
                    with_slot(|slot: &mut VoiceSlot| slot.release(self.token));
                }
                VoiceEffect::StartEngine => self.start_engine(),
                // Shared: a stop that settles at once clears text through these.
                VoiceEffect::StopAndSend => self.hosts.borrow().stop(),
                VoiceEffect::AbortEngine => self.hosts.borrow().abort(),
                VoiceEffect::ClearEngineText => self.hosts.borrow().reset(),
                VoiceEffect::ArmFinalizeWatchdog => {
                    let weak = Rc::downgrade(self);
                    arm_watchdog(move || {
                        if let Some(context) = weak.upgrade() {
                            context.apply(VoiceEvent::WatchdogExpired);
                        }
                    });
                }
                VoiceEffect::ClearFinalizeWatchdog => clear_watchdog(),
                VoiceEffect::Commit(text) => {
                    self.on_transcript.call(text.clone());
                    self.on_live_transcript.call(None);
                }
                VoiceEffect::DiscardRecording => {
                    self.on_discard.call(());
                    self.on_live_transcript.call(None);
                }
                VoiceEffect::SubmitDraft => self.on_submit.call(()),
                VoiceEffect::ClearError => self.error.borrow_mut().set(None),
                // The engine's own caption is written where the engine reports
                // it; a refusal from the machine carries no text of its own.
                VoiceEffect::ShowError(_) => {}
            }
        }
        self.on_state_change.call(self.machine().state());
    }

    /// Start the engine, with a sink that reaches this composer weakly: the
    /// engine outlives the tap that built it, and a composer that unmounted must
    /// not be written to.
    #[cfg(target_arch = "wasm32")]
    fn start_engine(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let sink: EngineSink = Rc::new(move |event| {
            if let Some(context) = weak.upgrade() {
                context.on_engine_event(event);
            }
        });
        let reader = self
            .read_context
            .clone()
            .unwrap_or_else(ContextReader::empty);
        let keyterms = crate::voice::keyterm_source(reader.into_source());
        self.hosts.borrow_mut().start(
            (self.engine)(),
            (self.language)(),
            self.pump.clone(),
            keyterms,
            sink,
        );
    }

    /// Fold one engine report into the machine and perform what it returns.
    #[cfg(target_arch = "wasm32")]
    fn on_engine_event(self: &Rc<Self>, event: EngineEvent) {
        let event = match event {
            EngineEvent::Live => VoiceEvent::Live,
            EngineEvent::Settled => VoiceEvent::Settled,
            EngineEvent::Failed(caption) => {
                self.error.borrow_mut().set(Some(caption));
                VoiceEvent::Failed
            }
            EngineEvent::Transcript {
                settled,
                hypothesis,
            } => {
                self.machine
                    .borrow_mut()
                    .write()
                    .apply_transcript(&settled, &hypothesis);
                if self.machine().state().is_dictating() {
                    self.on_live_transcript.call(Some(LiveTranscript {
                        settled,
                        hypothesis,
                    }));
                }
                return;
            }
        };
        self.apply(event);
    }

    /// A build with no browser has no engine to start.
    ///
    /// The contract it would fulfil is still read here, so a host build cannot
    /// let `language`, `pump` or `read_context` rot unnoticed: `read_context` in
    /// particular is the seam the keyterm screen context arrives through.
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::unused_self)]
    fn start_engine(&self) {
        let _ = ((self.language)(), &self.pump, &self.read_context);
    }
}
