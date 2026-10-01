//! The mic: the DOM the oracles read, and the effects the state machine asks for.
//!
//! Ports `apps/web/src/components/MobileVoiceInput.tsx`. Every rule about what a
//! tap does lives in `crate::voice::state`; this file renders the four states,
//! performs the effects that machine returns, and hands the transcript to the
//! composer, which paints it. The engine name in `data-engine` comes from
//! `crate::voice::engine` and the value in `data-state` from the machine; both
//! are the strings the Playwright oracles select on.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

use super::md::{ButtonVariant, IconButton, IconButtonSize};
#[cfg(not(target_arch = "wasm32"))]
use super::mobile_voice_dom::warm_mic_for;
use super::mobile_voice_dom::{
    EngineHosts, bool_attr, choose_engine, is_secure_context, keep_keyboard, mic_icon, mic_label,
    read_config,
};
use super::mobile_voice_shell::use_voice_shell_controls;
use super::mobile_voice_watchdog::{arm_watchdog, clear_watchdog};
use crate::pump::{Pump, use_store};
#[cfg(target_arch = "wasm32")]
use crate::voice::deepgram_engine::{EngineEvent, EngineSink};
use crate::voice::engine::{Engine, start_refusal};
use crate::voice::keyterms::ContextReader;
use crate::voice::ownership::{VoiceSlot, with_slot};
use crate::voice::state::LiveTranscript;
use crate::voice::state::{VoiceEffect, VoiceEvent, VoiceMachine, VoiceState};

/// The composer this mic belongs to.
#[component]
pub fn MobileVoiceInput(
    /// The session whose composer owns the mic.
    owner_id: String,
    /// Whether this composer is the one that may record.
    #[props(default)]
    active: bool,
    /// The composer is recording, or on its way to.
    on_active_change: EventHandler<bool>,
    /// Final words to insert.
    on_transcript: EventHandler<String>,
    /// The interim update to paint, or `None` to stop painting.
    on_live_transcript: EventHandler<Option<LiveTranscript>>,
    /// The operator threw the recording away.
    on_discard: EventHandler<()>,
    /// The live terminal context the recognizer is biased with, asked once per
    /// socket open.
    #[props(default)]
    read_context: Option<ContextReader>,
) -> Element {
    let pump = use_store();
    let machine = use_signal(VoiceMachine::default);
    let error = use_signal(|| Option::<String>::None);
    let engine = use_signal(|| Engine::Unavailable);
    let deepgram_configured = use_signal(|| false);
    let language = use_signal(|| "en".to_owned());
    let hosts = use_hook(|| Rc::new(RefCell::new(EngineHosts::new())));

    // The claim token is this mount's identity for the page's voice slot: two
    // composers for one session are distinguishable only by the token.
    let (session_id, token) = use_hook(|| {
        let token = with_slot(|slot: &mut VoiceSlot| slot.issue_token());
        (owner_id.clone(), token)
    });

    {
        let pump = pump.clone();
        use_effect(move || read_config(pump.clone(), deepgram_configured, language));
    }

    {
        let mut engine = engine;
        use_effect(move || {
            engine.set(choose_engine(deepgram_configured()));
        });
    }

    // Everything the effect list needs, in one place, so a callback raised from
    // a timer performs the same work as one raised from a tap.
    let context = use_hook(|| {
        Rc::new(ComposerContext {
            machine: RefCell::new(machine),
            error: RefCell::new(error),
            engine,
            language,
            hosts: hosts.clone(),
            pump: pump.clone(),
            session_id: session_id.clone(),
            token,
            on_active_change,
            on_transcript,
            on_live_transcript,
            on_discard,
            read_context,
        })
    });

    // A pane switch commits what was settled and drops the hypothesis.
    {
        let context = context.clone();
        use_effect(move || {
            if !active {
                context.apply(VoiceEvent::Deactivated);
            }
        });
    }

    // Unmounting is the other way a recording ends, and the one a covered
    // composer takes: the drawer unmounts the dock rather than deactivating it,
    // so the machine above never hears about it. Nothing on the page owns the
    // microphone once this instance is gone, the page's voice slot is still this
    // instance's — a claim only its holder may release — and the composer that
    // painted a hypothesis over its draft is the one that can restore it.
    {
        let context = context.clone();
        use_drop(move || context.finish());
    }

    let state = context.machine().state();
    let owns = with_slot(|slot: &mut VoiceSlot| slot.owns(token));
    let caption = context.error();

    // The tap and a shell action (a controller's mic button) perform the same
    // work through this one door, so both reach the same refusal and the same
    // voice-slot claim.
    let toggle_recording = use_voice_shell_controls(Rc::clone(&context), active);
    let toggle = {
        let toggle_recording = Rc::clone(&toggle_recording);
        move |_event: MouseEvent| toggle_recording()
    };

    rsx! {
        div {
            class: "voice-input voice-input--inline",
            "data-testid": "mobile-voice-input",
            "data-state": state.data_state(),
            "data-owner-active": if owns { "true" } else { "false" },
            "data-engine": (context.engine)().data_engine(),
            if let Some(caption) = caption {
                div {
                    class: "voice-caption voice-caption--error",
                    "data-testid": "voice-caption",
                    span { class: "voice-caption__error", "Mic error: {caption}" }
                }
            }
            div { class: "voice-input__cluster",
                if state.is_dictating() && owns {
                    IconButton {
                        icon: "close",
                        label: "Discard recording",
                        variant: ButtonVariant::Ghost,
                        size: IconButtonSize::IconLg,
                        class: "voice-fab voice-fab--discard",
                        "data-testid": "voice-discard",
                        "type": "button",
                        onmousedown: keep_keyboard,
                        onclick: {
                            let context = context.clone();
                            move |_event: MouseEvent| context.discard()
                        },
                    }
                }
                IconButton {
                    icon: mic_icon(state),
                    label: mic_label(state),
                    variant: ButtonVariant::Secondary,
                    size: IconButtonSize::IconLg,
                    class: "voice-fab",
                    "data-testid": "voice-mic",
                    "type": "button",
                    "data-recording": bool_attr(state == VoiceState::Listening),
                    "data-starting": bool_attr(state == VoiceState::Starting),
                    "data-finalizing": bool_attr(state == VoiceState::Finalizing),
                    "aria-busy": (state == VoiceState::Starting).then_some("true"),
                    onmousedown: keep_keyboard,
                    onclick: toggle,
                }
            }
        }
    }
}

/// Everything one mic's callbacks need, so a timer and a tap perform the same
/// work through one door.
pub(super) struct ComposerContext {
    /// A Dioxus signal handle needs `&mut` to write, and this struct is shared
    /// by every callback a recording spawns, so the two written signals sit
    /// behind their own borrows.
    machine: RefCell<Signal<VoiceMachine>>,
    error: RefCell<Signal<Option<String>>>,
    engine: Signal<Engine>,
    language: Signal<String>,
    hosts: Rc<RefCell<EngineHosts>>,
    pump: Pump,
    session_id: String,
    token: u64,
    on_active_change: EventHandler<bool>,
    on_transcript: EventHandler<String>,
    on_live_transcript: EventHandler<Option<LiveTranscript>>,
    on_discard: EventHandler<()>,
    read_context: Option<ContextReader>,
}

impl ComposerContext {
    /// The machine as it stands, for a reader outside this module.
    pub(super) fn machine(&self) -> VoiceMachine {
        self.machine.borrow().read().clone()
    }

    fn error(&self) -> Option<String> {
        self.error.borrow().read().clone()
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
    fn apply(self: &Rc<Self>, event: VoiceEvent) {
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
        self.warm_mic();
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
                VoiceEffect::StopAndSend => self.hosts.borrow_mut().stop(),
                VoiceEffect::AbortEngine => self.hosts.borrow_mut().abort(),
                VoiceEffect::ClearEngineText => self.hosts.borrow_mut().reset(),
                VoiceEffect::ArmFinalizeWatchdog => self.arm_watchdog(),
                VoiceEffect::ClearFinalizeWatchdog => self.clear_watchdog(),
                VoiceEffect::Commit(text) => {
                    self.on_transcript.call(text.clone());
                    self.on_live_transcript.call(None);
                }
                VoiceEffect::DiscardRecording => {
                    self.on_discard.call(());
                    self.on_live_transcript.call(None);
                }
                // The engine's own caption is written where the engine reports
                // it; a refusal from the machine carries no text of its own.
                VoiceEffect::ShowError(_) | VoiceEffect::ClearError => {}
            }
        }
        self.on_active_change
            .call(self.machine().state().is_dictating());
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

    fn arm_watchdog(&self) {
        arm_watchdog();
    }

    fn clear_watchdog(&self) {
        clear_watchdog();
    }
}
