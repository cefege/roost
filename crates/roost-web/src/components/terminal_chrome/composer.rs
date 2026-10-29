//! The shared Rust/UI composer for terminal input, attachments and dictation.
//! The pane owns its draft, submission and key tray; this component places them.
//! Drafts and transient status float above the reserved field-plus-footer height
//! rather than resizing the PTY, which would duplicate inline TUI history.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::html::ModifiersInteraction as _;
use dioxus::prelude::*;

use super::attachment_picker::{AttachmentInput, ChosenFile};
use super::composer_attachments::{
    StagedAttachments, clear_staged_files, compose_terminal_submission, send_button_state,
    staged_files_block_send, upload_send_state,
};
use super::composer_claim::use_viewport_claim;
use super::composer_dictation::{
    GhostMirror, SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use super::composer_drafts::{get_composer_draft, save_composer_draft};
use super::composer_key_tray::{ComposerKeyTray, KeyTray};
use super::composer_placement::ComposerPlacement;
use super::dom;
use super::pane_geometry_dom::PaneDockHandle;
use crate::components::deck::deck_dom;
use crate::pump::use_pump;
use crate::voice::keyterms::ContextReader;
use crate::voice::send_gate::SendGate;

use crate::components::layout::portal::Portal;
use crate::components::terminal::pane_handle::PaneHandle;
use crate::components::ui::{
    ButtonSize, ButtonVariant, FieldError, Icon, IconButton, IconType, InputPrompt,
    InputPromptFooter, InputPromptSubmit, InputPromptTextarea, InputPromptTools,
};

/// What a submission is doing, for the status line under the field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubmissionStatus {
    /// The sentence the status line shows, or `None` when it shows nothing.
    pub message: Option<String>,
    /// Whether a refusal put the draft back, so the user can fix it and resend.
    pub restore_draft: bool,
}

/// Whether this Enter press is a submission rather than a newline.
///
/// A composition session owns its own Return — an IME candidate window closed by
/// a keydown would commit the wrong candidate — and a touch device's Return is
/// the soft keyboard's newline key, so submitting there would eat a line the
/// operator meant to break. Both refusals are silent: the newline is what the
/// key was for. Ports `TerminalComposeButton.tsx:359-370`.
fn enter_submits(event: &KeyboardEvent) -> bool {
    !event.is_composing() && !deck_dom::is_touch_device()
}

/// The composer for one session.
#[component]
pub fn TerminalComposer(
    session_id: String,
    handle: PaneHandle,
    #[props(default)] active: bool,
    #[props(default)] placement: ComposerPlacement,
    #[props(default)] pending: bool,
    on_attach: EventHandler<Vec<ChosenFile>>,
    staged_files: Signal<Vec<ChosenFile>>,
    on_retry_upload: EventHandler<usize>,
    /// pixels: the pane translates its display by it so a growing draft never
    /// takes rows from the grid. Only the pane placement has a flow to push.
    #[props(default)]
    on_measured: EventHandler<u32>,
    /// The live terminal context dictation is biased with, from whoever owns the
    /// renderer. `None` leaves the recognizer with nothing to bias.
    #[props(default)]
    read_context: Option<ContextReader>,
    /// The pane placement's measurement handle. `None` on the portaled dock,
    /// whose height the shell measures for itself.
    #[props(default)]
    dock_handle: Option<PaneDockHandle>,
    /// The TV input tray's terminal keys. `Some` puts a keys toggle in the bar
    /// and, while open, the keys above it.
    #[props(default)]
    key_tray: Option<KeyTray>,
) -> Element {
    let pump = use_pump();
    let mut draft = use_signal(|| {
        let core = pump.core();
        let core = core.borrow();
        get_composer_draft(core.storage(), &session_id)
    });
    let mut status = use_signal(SubmissionStatus::default);
    let mut voice_error = use_signal(|| Option::<String>::None);
    let mut field = use_signal(|| Option::<Rc<MountedData>>::None);
    let mut file_input = use_signal(|| Option::<Rc<MountedData>>::None);
    // What the mic owns while it records: the draft it started from, and where
    // the untrusted tail begins. The field carries the tail so the mirror can
    // cover it exactly, which is why what is STORED is asked for separately.
    let dictation = use_dictation(draft);
    // Set by a submission for the one write that clears the draft: the field
    // keeps its grown height through the gesture that sent it, and shrinks once
    // the submission is behind it, as v2's `sendLine` re-grows only after its
    // admission settles. A click that repaints the terminal must see the
    // composer at the geometry the reader clicked at.
    let hold_height = use_hook(|| Rc::new(Cell::new(false)));

    // A controlled textarea is `rows="1"`, so without a re-measure after every
    // write — a keystroke, a restored draft, a dictated tail — the field stays
    // one line tall and the dock never overflows its resting row. Reading both
    // signals is the subscription: the field handle arrives after the first
    // effect pass, and a draft that has not changed still has to be measured.
    let held = hold_height.clone();
    use_effect(move || {
        let _text = draft();
        let _mounted = field();
        if held.get() {
            return;
        }
        if let Some(field) = field.peek().as_ref() {
            dom::auto_grow(field);
            dom::keep_caret_visible(field);
        }
    });

    // The draft outlives this instance: a compact/desktop swap, a pane switch
    // and a reload must all find it, so every edit is written through. The
    // mount write is the value that was just read, so it changes nothing.
    // What is written is the draft without an unproven hypothesis: the field
    // hands its guess to the mirror, but a store hands it back as ordinary
    // text, and a guess is not the operator's unfinished work.
    let draft_key = session_id.clone();
    let saved = dictation.clone();
    let core = pump.core();
    use_effect(move || {
        let text = saved.persisted();
        let core = core.borrow();
        save_composer_draft(core.storage(), &draft_key, &text);
    });

    // The portaled dock publishes its height to the shell; the pane dock does
    // not, because it is not in a flow the shell reserves. Visibility decides
    // the hold, and the drawer read below is that visibility: the drawer
    // covers the dock and uncovers it without unmounting it, so a claim that
    // outlived the drawer left the shell reserving for a composer nobody can
    // see — and a covered dock's detached element measures 0.
    let on_screen =
        super::composer_gate::dock_on_screen(placement, super::composer_gate::use_drawer_open());
    let slot = use_viewport_claim(placement, on_screen);

    // The pane dock measures itself once its children exist, which is after the
    // first render: a ref runs before the box and the field are connected.
    let measured_dock = use_hook(|| dock_handle.clone());
    use_effect(move || {
        if let Some(handle) = measured_dock.as_ref() {
            handle.refresh();
        }
    });

    let disabled = !active || pending;
    // The ghost is painted from the same split the voice state machine hands the
    // mic, so the field and the mirror can never disagree about where the
    // hypothesis starts.
    let read_context = use_dictation_context(read_context, draft);
    let ghost = dictation.ghost();
    let status_message = status().message.clone();
    let (upload_pending, upload_failed) = upload_send_state(&staged_files());
    // The submission is one piece of work; the button and the Enter key differ
    // only in which event carried it.
    let mut send = {
        let handle = handle.clone();
        let hold_height = hold_height.clone();
        move || {
            if !active || pending {
                return;
            }
            let staged = staged_files();
            if staged_files_block_send(&staged) {
                return;
            }
            let submission = compose_terminal_submission(&staged, &draft());
            handle.send_text(&submission, true);
            clear_staged_files(staged_files);
            hold_height.set(true);
            draft.set(String::new());
            status.set(SubmissionStatus::default());
            let release = hold_height.clone();
            spawn(async move {
                // A timer, not a microtask: a microtask runs between two
                // listeners of the same click.
                crate::components::terminal::dom::sleep_ms(0).await;
                release.set(false);
                if let Some(field) = field.peek().as_ref() {
                    dom::auto_grow(field);
                    dom::keep_caret_visible(field);
                }
            });
            tracing::debug!(
                target: "terminal",
                bytes = submission.len(),
                "composer submitted"
            );
        }
    };
    // While a recording waits for its last words, Send queues with the mic,
    // which submits through `submit_queued` once they land.
    let mut press_send = {
        let dictation = dictation.clone();
        let mut send = send.clone();
        move || {
            if staged_files_block_send(&staged_files()) {
                return;
            }
            match dictation.send_gate() {
                SendGate::Submit => send(),
                SendGate::AfterFinalize => dictation.send_after_finalize(),
                SendGate::Withheld => {}
            }
        }
    };
    let submit = {
        let mut press_send = press_send.clone();
        move |_event: MouseEvent| press_send()
    };
    let submit_queued = move |()| send();
    // A control in the pill must not steal focus from an already-focused field:
    // if Escape or an outside interaction blurred it, the click must not
    // reopen the soft keyboard.
    let keep_keyboard = |event: MouseEvent| event.prevent_default();
    let open_picker =
        move |event| super::attachment_picker::open_attachment_picker(event, active, file_input);
    // The session id is still needed after the submit closure has taken it.
    let owner_id = use_hook(move || session_id.clone());
    let dictation_for_keys = dictation.clone();
    let dictation_for_send = dictation.clone();
    let mic_active = dictation.clone();
    let dictation_for_mic_control = mic_active.clone();
    let (send_disabled, send_title) = send_button_state(
        !dictation_for_send.send_gate().offers_send() || !active || pending,
        upload_pending,
        upload_failed,
    );
    let on_key_down = move |event: KeyboardEvent| {
        // Send stops a live recording and waits for its full transcript; a
        // stopped recording also queues Send until those words are in the draft.
        // Uploaded paths are prefixed to the draft once every transfer settles.
        if event.key() == Key::Named(NamedKey::Enter)
            && !event.modifiers().contains(Modifiers::SHIFT)
            && dictation_for_keys.send_gate().offers_send()
            && enter_submits(&event)
            && dom::enter_targets_composer_or_staged_row(&event)
        {
            event.prevent_default();
            press_send();
            return;
        }
        // Escape removes staged files; otherwise focus returns to the terminal.
        if event.key() == Key::Named(NamedKey::Escape) {
            event.prevent_default();
            if !staged_files().is_empty() {
                clear_staged_files(staged_files);
            } else if let Some(field) = field.peek().as_ref() {
                dom::blur(field);
            }
        }
    };

    let mounted_slot = slot.clone();
    // Nothing renders for a dock the drawer has covered: a composer under it
    // is a control the reader cannot reach and cannot dismiss, and its field
    // would hold the soft keyboard's focus with nothing on screen to type into.
    if !on_screen {
        return rsx! {};
    }

    let dock = rsx! {
        div {
            class: "group/dock pointer-events-none fixed z-56 flex flex-col items-stretch gap-2 min-w-0 font-sans @container/term-chat-pane left-[calc(var(--roost-main-left,0px)_+_env(safe-area-inset-left,0px)_+_0.5rem)] right-[calc(env(safe-area-inset-right,0px)_+_0.5rem)] bottom-(--term-chat-dock-offset) data-[placement=viewport]:tv:left-[calc(var(--roost-main-left,0px)_+_var(--tv-overscan-inline)_+_0.5rem)] data-[placement=viewport]:tv:right-[calc(var(--tv-overscan-inline)_+_0.5rem)] data-[placement=viewport]:tv:bottom-[calc(var(--term-chat-dock-offset)_+_var(--tv-overscan-block))] data-[placement=pane]:relative data-[placement=pane]:left-auto data-[placement=pane]:right-auto data-[placement=pane]:bottom-auto data-[placement=pane]:mx-2 data-[placement=pane]:mb-2 data-[placement=pane]:h-[var(--term-chat-pane-rest,auto)] data-[placement=pane]:max-h-[calc(100%_-_0.5rem)] data-[placement=pane]:flex-none data-[placement=pane]:justify-end data-[placement=pane]:self-stretch data-[placement=pane]:data-[size-constrained]:justify-start data-[placement=pane]:data-[size-constrained]:overflow-auto *:shrink-0",
            "data-testid": "mobile-chat-input",
            "data-open": "true",
            "data-placement": placement.as_str(),
            "data-active": if active { "true" } else { "false" },
            aria_hidden: (!active).then_some("true"),
            onkeydown: on_key_down,
            onmounted: move |event: MountedEvent| {
                if let Some(handle) = dock_handle.as_ref() {
                    handle.attach(event.data(), on_measured);
                    return;
                }
                // The portaled dock is not in a flow to push, so the shell
                // reserves ITS measured height instead. A constant would be
                // wrong the moment the dock grew.
                if let Some(slot) = mounted_slot.as_ref() {
                    slot.attach(event.data());
                }
            },
            div { class: "relative min-w-0",
                if let Some(tray) = key_tray.as_ref().filter(|tray| tray.open) {
                    {tray.keys.clone()}
                }
            InputPrompt {
                class: "pointer-events-auto relative animate-voice-dock-enter motion-reduce:animate-none @container/term-chat-pane",
                "data-testid": "chat-box",
                "data-composer-part": "box",
                StagedAttachments {
                    staged_files,
                    on_retry: on_retry_upload,
                    compact_image_uploads: true,
                    inline_transfer_scope: owner_id.clone(),
                    visible: active,
                }
                div {
                    class: "relative flex w-full min-w-0",
                    "data-testid": "chat-field",
                InputPromptTextarea {
                    value: draft,
                    class: "relative z-1 w-full min-w-0 field-sizing-fixed overflow-y-auto max-h-[min(calc(var(--term-chat-line-height)_*_var(--term-chat-field-max-lines)_+_6_*_var(--spacing)),calc(var(--term-chat-pane-height,calc(100dvh_-_max(var(--kb-offset,0px),0px)))_*_var(--term-chat-field-room-share)))] placeholder:whitespace-nowrap data-[ghosted=true]:text-transparent data-[ghosted=true]:caret-foreground data-[ghosted=true]:[scrollbar-width:none] data-[ghosted=true]:[&::-webkit-scrollbar]:hidden disabled:cursor-auto disabled:opacity-100",
                    "data-testid": "chat-input",
                    "data-composer-part": "input",
                    "data-ghosted": ghost.has_ghost().then_some("true"),
                    aria_label: "Terminal input",
                    rows: "1",
                    placeholder: dictation.placeholder(),
                    disabled,
                    submit_on_enter: false,
                    on_mounted: move |event: MountedEvent| field.set(Some(event.data())),
                    on_input: move |_next: String| {
                        dictation.forget_provisional();
                        status.set(SubmissionStatus::default());
                    },
                }
                if ghost.has_ghost() {
                    GhostMirror { ghost: ghost.clone() }
                }
                }
                InputPromptFooter { class: "flex-wrap",
                    InputPromptTools { class: "min-w-0 flex-wrap",
                        IconButton {
                            icon: IconType::Paperclip,
                            label: "Attach files",
                            title: "Attach files",
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::IconSm,
                            "data-testid": "chat-attach",
                            "aria-disabled": disabled.then_some("true"),
                            onmousedown: keep_keyboard,
                            onclick: open_picker,
                        }
                        if let Some(tray) = key_tray.clone() {
                            ComposerKeyTray { tray }
                        }
                    }
                    InputPromptTools { class: "min-w-0 flex-wrap justify-end",
                        div { class: if active { "contents" } else { "invisible" },
                            VoiceControl {
                                owner_id: owner_id.clone(),
                                active: active && !pending,
                                read_context: Some(read_context.clone()),
                                binding: SharedBinding(dictation_for_mic_control.clone()),
                                on_submit: submit_queued,
                                on_error: move |error| voice_error.set(error),
                            }
                        }
                        InputPromptSubmit {
                            disabled: send_disabled,
                            aria_label: "Send to terminal",
                            title: send_title,
                            test_id: "chat-send",
                            onmousedown: keep_keyboard,
                            onclick: submit,
                            Icon { icon: IconType::ArrowUp, class: "size-4" }
                        }
                }
            }
            }
            }
            AttachmentInput {
                on_chosen: move |chosen| on_attach.call(chosen),
                onmounted: move |event: MountedEvent| file_input.set(Some(event.data())),
            }
            if let Some(message) = status_message {
                div { role: "status", "{message}" }
            }
            if let Some(error) = voice_error() {
                FieldError { test_id: "voice-caption", "Mic error: {error}" }
            }
        }
    };
    // The viewport dock is portaled out of the deck, as v2's is to `<body>`: the
    // deck is transformed and clips its overflow, so a fixed dock inside it is
    // placed by the deck — and scrolled with it whenever the focused field asks
    // its scroll ancestors to reveal the caret.
    match placement {
        ComposerPlacement::Viewport => rsx! {
            Portal { {dock} }
        },
        ComposerPlacement::Pane => dock,
    }
}
