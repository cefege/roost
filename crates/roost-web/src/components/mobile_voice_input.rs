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
use crate::pump::use_store;
use crate::voice::engine::Engine;
use crate::voice::keyterms::ContextReader;
use crate::voice::ownership::{VoiceSlot, with_slot};
use crate::voice::state::{LiveTranscript, VoiceEvent, VoiceMachine, VoiceState};
use libero::components::{ActionIcon, Alert, Box, Flex};
use libero::sx::sx;

/// The recording controls, laid out against the terminal composer's measured
/// container so the dock retains its established compact and narrow-pane geometry.
#[component]
pub fn MobileVoiceInput(
    /// The session whose composer owns the mic.
    owner_id: String,
    /// Whether this composer is the one that may record.
    #[props(default)]
    active: bool,
    /// Inline control for a composer footer; the terminal keeps its FAB layout.
    #[props(default)]
    compact: bool,
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
    let root_sx = use_hook(|| sx().display("contents"));
    let controls_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("xs")
            .grid_column("2")
            .grid_row("2")
            .selector(
                "[data-compact='false'] &",
                sx().grid_column("3").grid_row("1"),
            )
            .container("term-chat-pane")
            .container_query(
                "term-chat-pane",
                "(max-width: 220px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_column("2").grid_row("2").justify_self("center"),
                ),
            )
            .container_query(
                "term-chat-pane",
                "(max-width: 104px)",
                sx().selector(
                    "[data-compact='false'] &",
                    sx().grid_column("1").grid_row("3"),
                ),
            )
    });
    let compact_button_sx = use_hook(|| {
        sx().width("2rem")
            .height("2rem")
            .border_radius("9999rem")
            .transition("all 150ms")
            .selector("&[data-recording='true']", sx().border_radius("lg"))
            .selector("&[data-starting='true']", sx().opacity("0.7"))
            .selector(
                "&[data-starting='true'] > svg",
                sx().with("animation", "roost-voice-starting-spin 1s linear infinite"),
            )
            .selector("&[data-finalizing='true']", sx().opacity("0.6"))
            .selector(
                "&[data-finalizing='true'] > svg",
                sx().with(
                    "animation",
                    "roost-voice-finalizing-pulse 2s cubic-bezier(.4,0,.6,1) infinite",
                ),
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().transition("none")
                    .selector("& > svg", sx().with("animation", "none")),
            )
    });
    let mic_button_sx = use_hook(|| {
        sx().width("3.5rem")
            .height("3.5rem")
            .border_radius("9999rem")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Md))
            .transition("all 150ms")
            .selector("&[data-recording='true']", sx().border_radius("xl"))
            .selector("&[data-starting='true']", sx().opacity("0.7"))
            .selector(
                "&[data-starting='true'] > svg",
                sx().with("animation", "roost-voice-starting-spin 1s linear infinite"),
            )
            .selector("&[data-finalizing='true']", sx().opacity("0.6"))
            .selector(
                "&[data-finalizing='true'] > svg",
                sx().with(
                    "animation",
                    "roost-voice-finalizing-pulse 2s cubic-bezier(.4,0,.6,1) infinite",
                ),
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().transition("none")
                    .selector("& > svg", sx().with("animation", "none")),
            )
    });
    let compact_discard_sx =
        use_hook(|| sx().width("2rem").height("2rem").border_radius("9999rem"));
    let discard_sx = use_hook(|| {
        sx().width("2.5rem")
            .height("2.5rem")
            .border_radius("9999rem")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Md))
    });
    let compact_caption_sx = use_hook(|| {
        sx().with("order", "9999")
            .with("flex-basis", "100%")
            .min_width("0")
            .padding_inline("sm")
            .padding_block("xs")
            .font_size("xs")
    });
    let caption_sx = use_hook(|| {
        sx().position("absolute")
            .with("bottom", "100%")
            .left("0")
            .right("0")
            .margin_bottom("sm")
            .width("auto")
            .max_width("none")
            .white_space("pre-wrap")
            .border("0")
            .border_radius("xl")
            .padding_inline("md")
            .padding_block("sm")
            .font_size("md")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Lg))
            .pointer_events("none")
            .with(
                "animation",
                "voice-caption-enter 220ms cubic-bezier(0.2,0,0,1)",
            )
            .selector("[data-compact='true'] &", sx().right("4rem"))
            .selector(
                "[data-size-constrained] &",
                sx().position("static")
                    .with("left", "auto")
                    .with("right", "auto")
                    .with("bottom", "auto")
                    .with("grid-column", "1 / -1")
                    .with("grid-row", "auto")
                    .width("20rem")
                    .max_width("none"),
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().with("animation", "none"),
            )
    });

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
        use_effect(use_reactive((&active,), move |(active,)| {
            if !active {
                context.apply(VoiceEvent::Deactivated);
            }
        }));
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
        Box {
            component: "div",
            sx: root_sx,
            "data-testid": "mobile-voice-input",
            "data-state": state.data_state(),
            "data-owner-active": if owns { "true" } else { "false" },
            "data-engine": (context.engine)().data_engine(),
            if let Some(caption) = caption {
                Alert {
                    color: "error",
                    sx: if compact { compact_caption_sx } else { caption_sx },
                    title: "Mic error",
                    "data-testid": "voice-caption",
                    "Mic error: {caption}"
                }
            }
            Flex { direction: "row", sx: controls_sx,
                if state.is_dictating() && owns {
                    ActionIcon {
                        icon: pictogram_icons_lucide::x::outlined,
                        aria_label: "Discard recording",
                        variant: "standard",
                        color: "secondary",
                        size: if compact { "sm" } else { "lg" },
                        sx: if compact { compact_discard_sx } else { discard_sx },
                        "data-testid": "voice-discard",
                        "type": "button",
                        onmousedown: keep_keyboard,
                        onclick: {
                            let context = context.clone();
                            move |_event: MouseEvent| context.discard()
                        },
                    }
                }
                ActionIcon {
                    icon: mic_icon(state),
                    aria_label: mic_label(state),
                    variant: if state == VoiceState::Listening { "filled" } else { "standard" },
                    color: if state == VoiceState::Listening { "error" } else if compact { "text" } else { "primary" },
                    size: if compact { "sm" } else { "lg" },
                    sx: if compact { compact_button_sx } else { mic_button_sx },
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
