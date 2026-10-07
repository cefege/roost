//! The pane-local terminal input controller, minus the DOM: what one keydown,
//! paste, composition or focus transition on the pane's hidden textarea does.
//! `controller_dom` owns the textarea and its listeners and applies these
//! decisions; the key bytes come from `keys`. There is no browser-side VT core
//! and no shared focus singleton — every pane owns one controller.
//! Ports v2's `apps/web/src/renderer/terminalInputController.ts`.

use std::cell::RefCell;

use crate::input::chord::{
    AlternateKeys, KeyChord, KeyEventType, KeyKind, Modifiers, NamedKey,
};
use crate::input::keys::{
    FOCUS_REPORT_IN, FOCUS_REPORT_OUT, terminal_key_sequence_for_event,
};

/// The byte Meta+Backspace sends: readline's kill-to-line-start, which is what
/// that chord does in every macOS text field.
pub const META_BACKSPACE_BYTES: &str = "\x15";

/// One keyboard event, described without a DOM. Unlike `KeyChord` it keeps
/// the raw `key`, because the controller decides on spellings the encoder
/// never sees: the text-services keys, and the `c`/`v`/`a` shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalKeyEvent<'a> {
    /// `KeyboardEvent.key`.
    pub key: &'a str,
    /// The modifier state at the event.
    pub modifiers: Modifiers,
    /// `getModifierState("AltGraph")`.
    pub alt_graph: bool,
    /// `KeyboardEvent.isComposing`.
    pub is_composing: bool,
    pub event_type: KeyEventType,
    pub associated_text: Option<&'a str>,
    pub alternate_keys: AlternateKeys,
    pub key_override: Option<super::chord::NamedKey>,
}

impl<'a> TerminalKeyEvent<'a> {
    /// A key with no modifiers, outside any composition.
    pub const fn new(key: &'a str) -> Self {
        Self {
            key,
            modifiers: Modifiers::NONE,
            alt_graph: false,
            is_composing: false,
            event_type: KeyEventType::Press,
            associated_text: None,
            alternate_keys: AlternateKeys {
                shifted: None,
                base_layout: None,
                unshifted: None,
            },
            key_override: None,
        }
    }

    /// This event with `modifiers` held.
    pub const fn with_modifiers(mut self, modifiers: Modifiers) -> Self {
        self.modifiers = modifiers;
        self
    }

    /// The chord the encoder takes in legacy mode.
    pub fn chord(&self) -> KeyChord {
        self.chord_with_flags(0)
    }

    /// The physical keypad key is distinct when its key event has no text.
    pub fn chord_with_flags(&self, kitty_flags: u8) -> KeyChord {
        let keypad_key = kitty_flags & 1 != 0
            && self.key.chars().count() != 1
            && matches!(self.key_override, Some(NamedKey::Functional(57399..=57427)));
        let use_key_override = kitty_flags & 8 != 0 || keypad_key;
        KeyChord {
            kind: if use_key_override {
                self.key_override
                    .map(KeyKind::Named)
                    .unwrap_or_else(|| KeyKind::from_dom_key(self.key))
            } else {
                KeyKind::from_dom_key(self.key)
            },
            modifiers: self.modifiers,
            alt_graph: self.alt_graph,
            is_composing: self.is_composing,
        }
    }

    /// A dead key, an IME key or an unidentifiable key: the browser's text
    /// services finish these exactly once, through the textarea's `input`.
    fn is_text_services_key(&self) -> bool {
        matches!(self.key, "Dead" | "Process" | "Unidentified")
    }
}

/// What the pane does with one keydown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDownAction {
    /// Leave the event entirely to the browser: no `preventDefault`, no bytes.
    Browser,
    /// Focus the textarea (without scrolling) and let the platform paste run;
    /// its `ClipboardEvent` is admitted once through the paste path.
    FocusForPaste,
    /// `preventDefault`, then select the pane's own contents.
    SelectPane,
    /// `preventDefault`, then write these bytes to the PTY.
    Write(String),
    /// `preventDefault` and send nothing: the pane owns the key but it has no
    /// terminal encoding, and the browser must not scroll or activate UI.
    Consume,
}

/// Text the textarea committed. The textarea is emptied either way; `text`
/// is written to the PTY only when it is not empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextareaCommit {
    /// The committed text, possibly empty.
    pub text: String,
}

/// The fallback commit a `compositionend` leaves behind for one microtask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingComposition {
    token: u64,
    committed: String,
}

/// The state one pane's input controller carries across events.
#[derive(Debug, Default)]
pub struct InputControllerState {
    composing: bool,
    /// Bumped by every commit path, so exactly one of `input` and the
    /// `compositionend` fallback writes a composition's text.
    composition_token: u64,
    destroyed: bool,
    /// Focus as the APPLICATION last saw it. CSI I / CSI O report a transition,
    /// so a repeated focus event reports once.
    reported_focus: bool,
    /// Set across the force-focus dance's blur, which re-fires focus and is
    /// not the pane losing the keyboard.
    refocusing: bool,
}

impl InputControllerState {
    /// A live controller, unfocused, outside any composition.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `destroy` has run.
    pub const fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// Whether an IME composition is in flight.
    pub const fn is_composing(&self) -> bool {
        self.composing
    }

    /// Decide one keydown. `selection_has_text` is read only for Ctrl/Cmd+C,
    /// the one case that must know whether the document has a selection.
    pub fn key_down(
        &self,
        event: &TerminalKeyEvent<'_>,
        cursor_keys_application: bool,
        selection_has_text: impl FnOnce() -> bool,
    ) -> KeyDownAction {
        self.key_down_with_flags(event, cursor_keys_application, 0, selection_has_text)
    }

    /// Decide one keydown with active kitty progressive-enhancement flags.
    pub fn key_down_with_flags(
        &self,
        event: &TerminalKeyEvent<'_>,
        cursor_keys_application: bool,
        kitty_flags: u8,
        selection_has_text: impl FnOnce() -> bool,
    ) -> KeyDownAction {
        if self.destroyed || self.composing || event.is_composing || event.is_text_services_key() {
            return KeyDownAction::Browser;
        }
        let all_keys = kitty_flags & 8 != 0;
        let command =
            event.modifiers.meta || event.modifiers.super_key || event.modifiers.ctrl;
        if !all_keys && command && event.key.eq_ignore_ascii_case("c") && selection_has_text() {
            return KeyDownAction::Browser;
        }
        if !all_keys && command && event.key.eq_ignore_ascii_case("v") {
            return KeyDownAction::FocusForPaste;
        }
        if kitty_flags == 0
            && (event.modifiers.meta || event.modifiers.super_key)
            && !event.modifiers.ctrl
        {
            if event.key == "Backspace" {
                return KeyDownAction::Write(META_BACKSPACE_BYTES.to_string());
            }
            if event.key.eq_ignore_ascii_case("a") {
                return KeyDownAction::SelectPane;
            }
            return KeyDownAction::Browser;
        }
        match terminal_key_sequence_for_event(
            &event.chord_with_flags(kitty_flags),
            cursor_keys_application,
            kitty_flags,
            event.event_type,
            event.associated_text,
            event.alternate_keys,
        ) {
            Some(bytes) => KeyDownAction::Write(bytes),
            None => KeyDownAction::Consume,
        }
    }

    /// Touch navigation and focus recovery encode through exactly the
    /// physical-key path, without synthesizing a second DOM event that could
    /// duplicate IME input.
    pub fn dispatch_keydown(
        &self,
        event: &TerminalKeyEvent<'_>,
        cursor_keys_application: bool,
    ) -> Option<String> {
        self.dispatch_keydown_with_flags(event, cursor_keys_application, 0)
    }

    /// Dispatch a key event using the current kitty keyboard flags.
    pub fn dispatch_keydown_with_flags(
        &self,
        event: &TerminalKeyEvent<'_>,
        cursor_keys_application: bool,
        kitty_flags: u8,
    ) -> Option<String> {
        if self.destroyed {
            return None;
        }
        terminal_key_sequence_for_event(
            &event.chord_with_flags(kitty_flags),
            cursor_keys_application,
            kitty_flags,
            event.event_type,
            event.associated_text,
            event.alternate_keys,
        )
    }

    /// A composition began. The caller empties the textarea.
    pub fn composition_start(&mut self) {
        self.composing = true;
        self.composition_token += 1;
    }

    /// A composition ended. Chromium dispatches the final `input` in the same
    /// native turn and some Safari variants never do, so the caller defers
    /// `settle_composition` one microtask: `input` cancels it when it comes.
    pub fn composition_end(
        &mut self,
        data: Option<&str>,
        textarea_value: &str,
    ) -> PendingComposition {
        self.composing = false;
        self.composition_token += 1;
        PendingComposition {
            token: self.composition_token,
            committed: first_non_empty(data, textarea_value).to_string(),
        }
    }

    /// The deferred fallback: the commit, unless `input` or a newer
    /// composition already took it.
    pub fn settle_composition(
        &mut self,
        pending: &PendingComposition,
        textarea_value: &str,
    ) -> Option<TextareaCommit> {
        if self.destroyed || pending.token != self.composition_token {
            return None;
        }
        self.composition_token += 1;
        Some(TextareaCommit {
            text: first_non_empty(Some(textarea_value), &pending.committed).to_string(),
        })
    }

    /// One `input` event. Mid-composition input belongs to the IME.
    pub fn input(
        &mut self,
        is_composing: bool,
        textarea_value: &str,
        data: Option<&str>,
    ) -> Option<TextareaCommit> {
        if self.composing || is_composing {
            return None;
        }
        self.composition_token += 1;
        Some(TextareaCommit {
            text: first_non_empty(Some(textarea_value), data.unwrap_or_default()).to_string(),
        })
    }

    /// The textarea's real focus or blur. Returns the DECSET 1004 report to
    /// write, when the application asked for one and this is a transition the
    /// application has not been told about.
    pub fn focus_changed(
        &mut self,
        focused: bool,
        focus_events_enabled: bool,
    ) -> Option<&'static str> {
        if self.refocusing || self.reported_focus == focused {
            return None;
        }
        self.reported_focus = focused;
        tracing::debug!(target: "input", focused, "terminal focus reported");
        focus_events_enabled.then_some(if focused {
            FOCUS_REPORT_IN
        } else {
            FOCUS_REPORT_OUT
        })
    }

    /// Stop accepting events. A composition fallback still queued is cancelled.
    pub fn destroy(&mut self) {
        self.destroyed = true;
        self.composition_token += 1;
    }
}

/// A focus call the textarea refused: a detached pane racing cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusRefused;

/// The textarea as the force-focus dance drives it. `blur` and `focus` fire
/// the element's own listeners synchronously, exactly as the DOM does, so an
/// implementation must not hold the controller state borrowed across them.
pub trait FocusSurface {
    /// Whether the textarea is the document's active element.
    fn is_active(&self) -> bool;
    /// Blur the textarea.
    fn blur(&self) -> Result<(), FocusRefused>;
    /// Focus the textarea without scrolling.
    fn focus(&self) -> Result<(), FocusRefused>;
    /// Dispatch an explicit bubbling `focus` event at the textarea.
    fn dispatch_focus(&self) -> Result<(), FocusRefused>;
}

/// The blur-first focus dance. Focusing an already-active textarea fires no
/// focus event, so it is blurred first to guarantee a fresh one, and the
/// explicit dispatch keeps pane styling deterministic. Returns whether focus
/// landed on the textarea.
///
/// The blur is not the pane losing the keyboard, so focus reporting is muted
/// across it — and unmuted unconditionally, before any error can end the
/// dance: a guard left latched kills focus reporting for the pane's lifetime.
pub fn force_focus(state: &RefCell<InputControllerState>, surface: &impl FocusSurface) -> bool {
    if state.try_borrow().map_or(true, |state| state.destroyed) {
        return false;
    }
    let danced = (|| {
        if surface.is_active() {
            set_refocusing(state, true);
            let blurred = surface.blur();
            set_refocusing(state, false);
            blurred?;
        }
        surface.focus()?;
        if surface.is_active() {
            surface.dispatch_focus()?;
        }
        Ok::<(), FocusRefused>(())
    })();
    let landed = surface.is_active();
    tracing::debug!(target: "input", landed, completed = danced.is_ok(), "focus.force");
    landed
}

fn set_refocusing(state: &RefCell<InputControllerState>, refocusing: bool) {
    if let Ok(mut state) = state.try_borrow_mut() {
        state.refocusing = refocusing;
    }
}

/// JavaScript's `a || b` over strings: the first unless it is empty.
fn first_non_empty<'a>(first: Option<&'a str>, second: &'a str) -> &'a str {
    match first {
        Some(text) if !text.is_empty() => text,
        _ => second,
    }
}
