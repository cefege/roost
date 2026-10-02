//! The permanent terminal composer: the draft field, the submit action, the
//! attachment picker, and the submission status line. It grows OVER the terminal
//! rather than taking rows from it, because every PTY resize makes an inline
//! agent repaint, and a repaint in place duplicates the rows a height shrink
//! pushed into history.
//! Ports `apps/web/src/components/terminal/TerminalComposeButton.tsx`: its
//! markup, its `term-chat__*` classes (already in the eager `voice-input.css`),
//! its test ids, its draft retention, and its autogrow field. The one v2 piece
//! with no browser owner in this build is the native Selection guard around the
//! terminal's own text.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::html::ModifiersInteraction as _;
use dioxus::prelude::*;

use super::attachment_picker::{AttachmentInput, ChosenFile};
use super::composer_claim::use_viewport_claim;
use super::composer_dictation::{
    GhostMirror, SharedBinding, VoiceControl, use_dictation, use_dictation_context,
};
use super::composer_drafts::{get_composer_draft, save_composer_draft};
use super::dom;
use super::pane_geometry_dom::PaneDockHandle;
use crate::components::deck::deck_dom;
use crate::voice::keyterms::ContextReader;

use crate::components::layout::portal::Portal;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{ButtonVariant, IconButton, IconButtonSize};
use crate::components::terminal::pane_handle::PaneHandle;

/// Where the composer is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerPlacement {
    /// Portaled to the document, above the status bar, for the compact shell.
    #[default]
    Viewport,
    /// Inside the pane, for a viewport wide enough to have its own status bar.
    Pane,
}

impl ComposerPlacement {
    /// The `data-placement` spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewport => "viewport",
            Self::Pane => "pane",
        }
    }

    /// The dock's own positioning: the pane dock flows with the pane, and the
    /// viewport dock is pinned so the shell's bottom chrome can reserve for it.
    const fn position(self) -> &'static str {
        match self {
            Self::Viewport => "position: fixed;",
            Self::Pane => "position: relative;",
        }
    }
}

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

/// The dock's inline style: its positioning, plus the growth the pane
/// translates its display by.
fn dock_style(placement: ComposerPlacement, growth_px: u32) -> String {
    if growth_px == 0 {
        return placement.position().to_owned();
    }
    format!(
        "{} --term-chat-pane-growth: {growth_px}px;",
        placement.position()
    )
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
    /// How far this dock overflowed above its resting row, in pixels. The pane
    /// translates its display by it so a growing draft never takes rows from
    /// the grid. Zero for the portaled dock, which is not in the pane's flow.
    #[props(default)]
    growth_px: u32,
    /// Where the dock's measured growth is reported. Only the pane placement
    /// has a flow to push.
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
) -> Element {
    let compact = use_is_compact();
    let mut draft = use_signal(|| get_composer_draft(&session_id));
    let mut status = use_signal(SubmissionStatus::default);
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
            dom::scroll_to_end(field);
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
    use_effect(move || {
        let text = saved.persisted();
        save_composer_draft(&draft_key, &text);
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
    let value = draft();
    // The ghost is painted from the same split the voice state machine hands the
    // mic, so the field and the mirror can never disagree about where the
    // hypothesis starts.
    let read_context = use_dictation_context(read_context, draft);
    let ghost = dictation.ghost();
    let mic_visible = active;
    let status_message = status().message.clone();

    // The submission is one piece of work; the button and the Enter key differ
    // only in which event carried it.
    let mut send = {
        let handle = handle.clone();
        let hold_height = hold_height.clone();
        move || {
            if !active || pending {
                return;
            }
            let text = draft();
            if text.is_empty() {
                return;
            }
            handle.send_text(&text, true);
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
                    dom::scroll_to_end(field);
                }
            });
            tracing::debug!(
                target: "terminal",
                bytes = text.len(),
                "composer submitted"
            );
        }
    };
    let submit = {
        let mut send = send.clone();
        move |_event: MouseEvent| send()
    };
    // A control in the pill must not steal focus from an already-focused field:
    // if Escape or an outside interaction blurred it, the click must not
    // reopen the soft keyboard.
    let keep_keyboard = |event: MouseEvent| event.prevent_default();
    let open_picker = move |event: MouseEvent| {
        event.prevent_default();
        if !active {
            return;
        }
        if let Some(input) = file_input.peek().as_ref() {
            dom::open_file_chooser(input);
        }
    };
    // The session id is still needed after the submit closure has taken it.
    let owner_id = use_hook(move || session_id.clone());
    let dictation_for_keys = dictation.clone();
    let dictation_for_send = dictation.clone();
    let mic_active = dictation.clone();
    let dictation_for_mic_control = mic_active.clone();
    let on_key_down = move |event: KeyboardEvent| {
        // Dictation owns the field while it records: an Enter then would send a
        // half-heard sentence, and the soft keyboard's newline is what the key
        // was for.
        if event.key() == Key::Enter
            && !event.modifiers().contains(Modifiers::SHIFT)
            && !dictation_for_keys.blocks_send()
            && enter_submits(&event)
        {
            event.prevent_default();
            send();
            return;
        }
        // Escape only drops the field's focus, so the terminal's own shortcuts
        // resume routing.
        if event.key() == Key::Escape {
            event.prevent_default();
            if let Some(field) = field.peek().as_ref() {
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
            class: "term-chat__dock",
            "data-testid": "mobile-chat-input",
            "data-open": "true",
            "data-placement": placement.as_str(),
            "data-active": if active { "true" } else { "false" },
            aria_hidden: (!active).then_some("true"),
            style: dock_style(placement, growth_px),
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
            div {
                class: "term-chat__box",
                "data-testid": "chat-box",
                "data-compact": if compact { "true" } else { "false" },
                IconButton {
                    icon: "attach_file",
                    label: "Attach files",
                    title: "Attach files",
                    variant: ButtonVariant::Ghost,
                    size: IconButtonSize::IconLg,
                    class: "term-chat__ctl term-chat__attach",
                    "data-testid": "chat-attach",
                    "aria-disabled": disabled.then_some("true"),
                    onmousedown: keep_keyboard,
                    onclick: open_picker,
                }
                div {
                    class: "term-chat__field",
                    "data-testid": "chat-field",
                    textarea {
                        class: "term-chat__input",
                        "data-testid": "chat-input",
                        "data-ghosted": ghost.has_ghost().then_some("true"),
                        "aria-label": "Terminal input",
                        rows: "1",
                        placeholder: dictation.placeholder(),
                        disabled: disabled,
                        value: "{value}",
                        onmounted: move |event: MountedEvent| field.set(Some(event.data())),
                        oninput: move |event: FormEvent| {
                            // A keystroke ends the provisional paint: the words
                            // the operator typed are now the draft.
                            dictation.forget_provisional();
                            draft.set(event.value());
                            status.set(SubmissionStatus::default());
                        },
                        onkeydown: on_key_down,
                    }
                    if ghost.has_ghost() {
                        GhostMirror { ghost: ghost.clone() }
                    }
                }
                if mic_visible {
                    VoiceControl {
                        owner_id: owner_id.clone(),
                        active: active && !pending,
                        read_context: Some(read_context.clone()),
                        binding: SharedBinding(dictation_for_mic_control.clone()),
                    }
                }
                if !dictation_for_send.blocks_send() {
                IconButton {
                    icon: "send",
                    label: "Send to terminal",
                    variant: ButtonVariant::Default,
                    size: IconButtonSize::IconLg,
                    class: "term-chat__ctl term-chat__send",
                    "data-testid": "chat-send",
                    "aria-disabled": disabled.then_some("true"),
                    onmousedown: keep_keyboard,
                    onclick: submit,
                }
                }
            }
            AttachmentInput {
                on_chosen: move |chosen| on_attach.call(chosen),
                onmounted: move |event: MountedEvent| file_input.set(Some(event.data())),
            }
            if let Some(message) = status_message {
                div {
                    class: "term-chat__status",
                    role: "status",
                    "{message}"
                }
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
