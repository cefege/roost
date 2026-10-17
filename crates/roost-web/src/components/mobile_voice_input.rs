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

use super::mobile_voice_context::ComposerContext;
use super::mobile_voice_dom::{
    EngineHosts, bool_attr, choose_engine, keep_keyboard, mic_icon, mic_label, read_config,
};
use super::mobile_voice_send::{FinalizeSendDoor, use_finalize_send_door};
use super::mobile_voice_shell::use_voice_shell_controls;
use crate::components::ui::{Alert, AlertVariant, ButtonSize, ButtonVariant, IconButton, IconType};
use crate::pump::use_store;
use crate::voice::engine::Engine;
use crate::voice::keyterms::ContextReader;
use crate::voice::ownership::{VoiceSlot, with_slot};
use crate::voice::state::{LiveTranscript, VoiceEvent, VoiceMachine, VoiceState};

/// The mic FAB. Recording morphs it to a rounded square and switches its tone to
/// the destructive variant, so the state is readable without relying on colour
/// alone (the icon also changes). The class carries shape and motion only.
const VOICE_MIC_CLASS: &str = "size-14 rounded-full shadow-md transition-all data-[recording=true]:rounded-xl data-[starting=true]:opacity-70 data-[starting=true]:[&>svg]:animate-spin data-[finalizing=true]:opacity-60 data-[finalizing=true]:[&>svg]:animate-pulse";
/// The discard FAB: a smaller circle beside the mic, in the Secondary tone.
const VOICE_DISCARD_CLASS: &str = "size-10 rounded-full shadow-md";
/// The mic error caption: a destructive bubble above the composer, or in the dock
/// when the dock is size-constrained.
const VOICE_CAPTION_CLASS: &str = "pointer-events-none absolute bottom-full left-0 right-0 mb-2 max-w-none w-auto whitespace-pre-wrap rounded-xl border-0 bg-destructive px-4 py-3 text-base text-destructive-foreground shadow-lg animate-voice-caption-enter motion-reduce:animate-none group-data-[compact=true]/box:right-[calc(3.5rem_+_0.5rem)] group-data-[size-constrained]/dock:static group-data-[size-constrained]/dock:left-auto group-data-[size-constrained]/dock:right-auto group-data-[size-constrained]/dock:bottom-auto group-data-[size-constrained]/dock:col-span-full group-data-[size-constrained]/dock:row-auto group-data-[size-constrained]/dock:w-80 group-data-[size-constrained]/dock:max-w-none";

/// The composer this mic belongs to.
#[component]
pub fn MobileVoiceInput(
    /// The session whose composer owns the mic.
    owner_id: String,
    /// Whether this composer is the one that may record.
    #[props(default)]
    active: bool,
    /// Where the recording is, after every step it takes.
    on_state_change: EventHandler<VoiceState>,
    /// Final words to insert.
    on_transcript: EventHandler<String>,
    /// The interim update to paint, or `None` to stop painting.
    on_live_transcript: EventHandler<Option<LiveTranscript>>,
    /// The operator threw the recording away.
    on_discard: EventHandler<()>,
    /// Submit the draft: Send was pressed while this finalized, and it landed.
    #[props(default)]
    on_submit: EventHandler<()>,
    /// The owning composer's door for a Send pressed while this finalizes.
    #[props(default)]
    send_door: Option<FinalizeSendDoor>,
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
            on_state_change,
            on_transcript,
            on_live_transcript,
            on_discard,
            on_submit,
            read_context,
        })
    });
    use_finalize_send_door(send_door, &context);

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
            class: "contents",
            "data-testid": "mobile-voice-input",
            "data-state": state.data_state(),
            "data-owner-active": if owns { "true" } else { "false" },
            "data-engine": (context.engine)().data_engine(),
            if let Some(caption) = caption {
                Alert {
                    variant: AlertVariant::Destructive,
                    test_id: "voice-caption",
                    class: VOICE_CAPTION_CLASS,
                    span { class: "text-destructive-foreground", "Mic error: {caption}" }
                }
            }
            div { class: "[grid-column:2] group-data-[compact=true]/box:row-start-2 group-data-[compact=false]/box:[grid-column:3] group-data-[compact=false]/box:row-start-1 @max-[220px]/term-chat-pane:group-data-[compact=false]/box:[grid-column:2] @max-[220px]/term-chat-pane:group-data-[compact=false]/box:row-start-2 @max-[220px]/term-chat-pane:group-data-[compact=false]/box:justify-self-center @max-[104px]/term-chat-pane:group-data-[compact=false]/box:[grid-column:1] @max-[104px]/term-chat-pane:group-data-[compact=false]/box:row-start-3 flex items-center gap-2",
                if state.is_dictating() && owns {
                    IconButton {
                        icon: IconType::X,
                        label: "Discard recording",
                        variant: ButtonVariant::Secondary,
                        size: ButtonSize::IconLg,
                        class: Some(VOICE_DISCARD_CLASS.to_owned()),
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
                    variant: if state == VoiceState::Listening { ButtonVariant::Destructive } else { ButtonVariant::Default },
                    size: ButtonSize::IconLg,
                    class: Some(VOICE_MIC_CLASS.to_owned()),
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
