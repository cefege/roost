//! The pane input controller's decisions: which keydowns the pane owns and
//! the bytes they write, the exactly-once IME commit, and DECSET 1004 focus
//! reporting across the blur-first focus dance.
//!
//! Test names are v2's, from `apps/web/tests/renderer/terminalInputController.test.ts`.
//! The textarea is a fake with DOM focus semantics that fires its listeners
//! into the same `InputControllerState` the wasm adapter drives; the dance
//! itself is the production `force_focus`. Guards FAILURE-INDEX "Typing goes
//! nowhere on a fresh mount".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};

use roost_web_terminal::input::{
    FOCUS_REPORT_IN, FOCUS_REPORT_OUT, FocusSurface, InputControllerState, KeyDownAction,
    META_BACKSPACE_BYTES, Modifiers, TerminalKeyEvent, force_focus,
};

/// One pane: the controller state, its textarea and root, and what the PTY got.
struct Pane {
    state: RefCell<InputControllerState>,
    focus_events: bool,
    active: Cell<bool>,
    root_focused: Cell<bool>,
    blur_fails: Cell<bool>,
    data: RefCell<Vec<String>>,
}

impl Pane {
    fn new(focus_events: bool) -> Self {
        Self {
            state: RefCell::new(InputControllerState::new()),
            focus_events,
            active: Cell::new(false),
            root_focused: Cell::new(false),
            blur_fails: Cell::new(false),
            data: RefCell::new(Vec::new()),
        }
    }

    /// A keydown, as the adapter applies it.
    fn key(&self, event: TerminalKeyEvent<'_>, application: bool) -> KeyDownAction {
        let action = self.state.borrow().key_down(&event, application, || false);
        if let KeyDownAction::Write(bytes) = &action {
            self.data.borrow_mut().push(bytes.clone());
        }
        action
    }

    /// The textarea's focus listener.
    fn fire_focus_change(&self, focused: bool) {
        self.root_focused.set(focused);
        let report = self
            .state
            .borrow_mut()
            .focus_changed(focused, self.focus_events);
        if let Some(report) = report {
            self.data.borrow_mut().push(report.to_string());
        }
    }

    /// A real blur of the textarea by the page.
    fn blur(&self) {
        self.active.set(false);
        self.fire_focus_change(false);
    }

    /// A real focus of the textarea by the page.
    fn focus(&self) {
        self.active.set(true);
        self.fire_focus_change(true);
    }

    fn data(&self) -> Vec<String> {
        self.data.borrow().clone()
    }
}

impl FocusSurface for Pane {
    fn is_active(&self) -> bool {
        self.active.get()
    }

    fn blur(&self) -> Result<(), ()> {
        if self.blur_fails.get() {
            return Err(());
        }
        Pane::blur(self);
        Ok(())
    }

    fn focus(&self) -> Result<(), ()> {
        Pane::focus(self);
        Ok(())
    }

    fn dispatch_focus(&self) -> Result<(), ()> {
        self.fire_focus_change(true);
        Ok(())
    }
}

fn held(key: &str, modifiers: Modifiers) -> TerminalKeyEvent<'_> {
    TerminalKeyEvent::new(key).with_modifiers(modifiers)
}

const CTRL_ALT: Modifiers = Modifiers {
    shift: false,
    alt: true,
    ctrl: true,
    meta: false,
};

#[test]
fn encodes_physical_keys_synchronously_from_the_panes_application_mode() {
    let normal = Pane::new(false);
    assert_eq!(
        normal.key(TerminalKeyEvent::new("ArrowUp"), false),
        KeyDownAction::Write("\x1b[A".to_string()),
        "a written key is also a prevented one"
    );
    let application = Pane::new(false);
    application.key(TerminalKeyEvent::new("ArrowUp"), true);
    assert_eq!(application.data(), ["\x1bOA"]);
}

#[test]
fn emits_altgraph_text_without_a_ctrl_byte_or_alt_escape_prefix() {
    let pane = Pane::new(false);
    let mut euro = held("€", CTRL_ALT);
    euro.alt_graph = true;
    pane.key(euro, false);
    assert_eq!(pane.data(), ["€"]);
}

#[test]
fn commits_chromium_and_fallback_ime_sequences_exactly_once() {
    let pane = Pane::new(false);
    let mut state = pane.state.borrow_mut();
    // Chromium: compositionend, then the final input in the same turn, then
    // the deferred fallback — which the input has already cancelled.
    state.composition_start();
    let mut process = TerminalKeyEvent::new("Process");
    process.is_composing = true;
    assert_eq!(
        state.key_down(&process, false, || false),
        KeyDownAction::Browser
    );
    let pending = state.composition_end(Some("é"), "é");
    let committed = state
        .input(false, "é", Some("é"))
        .expect("the final input commits");
    assert_eq!(committed.text, "é");
    assert_eq!(state.settle_composition(&pending, ""), None);
    // Safari: no final input, so the fallback commits.
    state.composition_start();
    let pending = state.composition_end(Some("中"), "中");
    let fallback = state
        .settle_composition(&pending, "中")
        .expect("the fallback commits");
    assert_eq!(fallback.text, "中");
    assert_eq!(
        state.settle_composition(&pending, "中"),
        None,
        "and only once"
    );
}

#[test]
fn admits_one_native_paste_and_leaves_framing_normalization_to_the_caller() {
    // The platform paste is never prevented at keydown: its ClipboardEvent is
    // the one admission, through the pane's paste path.
    let pane = Pane::new(false);
    for modifiers in [
        Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        Modifiers {
            meta: true,
            ..Modifiers::NONE
        },
    ] {
        assert_eq!(
            pane.key(held("v", modifiers), false),
            KeyDownAction::FocusForPaste
        );
    }
    assert!(pane.data().is_empty());
}

#[test]
fn preserves_the_blur_focus_dance_and_removes_pane_local_listeners() {
    let pane = Pane::new(false);
    assert!(force_focus(&pane.state, &pane));
    assert!(pane.root_focused.get());
    // Already focused: the dance blurs first so a fresh focus event fires.
    assert!(force_focus(&pane.state, &pane));
    assert!(pane.root_focused.get());

    pane.state.borrow_mut().destroy();
    assert_eq!(
        pane.key(TerminalKeyEvent::new("x"), false),
        KeyDownAction::Browser
    );
    assert_eq!(
        pane.state
            .borrow()
            .dispatch_keydown(&TerminalKeyEvent::new("x"), false),
        None
    );
    assert!(
        !force_focus(&pane.state, &pane),
        "a destroyed pane never dances"
    );
    assert!(pane.data().is_empty());
}

#[test]
fn reports_real_focus_and_blur_as_csi_i_csi_o_when_the_app_asked_for_1004() {
    let pane = Pane::new(true);
    force_focus(&pane.state, &pane);
    assert_eq!(pane.data(), [FOCUS_REPORT_IN]);
    force_focus(&pane.state, &pane);
    assert_eq!(
        pane.data(),
        [FOCUS_REPORT_IN],
        "the dance's own blur is not focus lost"
    );

    pane.blur();
    assert_eq!(pane.data(), [FOCUS_REPORT_IN, FOCUS_REPORT_OUT]);
    pane.blur();
    assert_eq!(pane.data(), [FOCUS_REPORT_IN, FOCUS_REPORT_OUT]);
    pane.focus();
    assert_eq!(
        pane.data(),
        [FOCUS_REPORT_IN, FOCUS_REPORT_OUT, FOCUS_REPORT_IN]
    );
}

#[test]
fn stays_silent_on_focus_transitions_when_the_app_never_asked_for_1004() {
    let pane = Pane::new(false);
    force_focus(&pane.state, &pane);
    pane.blur();
    pane.focus();
    assert!(pane.data().is_empty());
    assert!(pane.root_focused.get());
}

#[test]
fn a_dance_whose_blur_fails_never_latches_the_focus_report_guard() {
    let pane = Pane::new(true);
    force_focus(&pane.state, &pane);
    // A detached pane racing cleanup: the dance's blur throws.
    pane.blur_fails.set(true);
    force_focus(&pane.state, &pane);
    pane.blur_fails.set(false);
    // The next REAL blur is the application's focus-lost, and must reach it.
    pane.blur();
    assert_eq!(pane.data(), [FOCUS_REPORT_IN, FOCUS_REPORT_OUT]);
}

#[test]
fn a_copy_chord_over_a_live_selection_is_the_browsers_and_without_one_is_ctrl_c() {
    let pane = Pane::new(false);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let state = pane.state.borrow();
    assert_eq!(
        state.key_down(&held("c", ctrl), false, || true),
        KeyDownAction::Browser
    );
    assert_eq!(
        state.key_down(&held("c", ctrl), false, || false),
        KeyDownAction::Write("\x03".to_string())
    );
    // macOS: Cmd+Backspace kills to line start; Cmd+A selects the pane; any
    // other Cmd shortcut stays the browser's.
    let meta = Modifiers {
        meta: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        state.key_down(&held("Backspace", meta), false, || false),
        KeyDownAction::Write(META_BACKSPACE_BYTES.to_string())
    );
    assert_eq!(
        state.key_down(&held("a", meta), false, || false),
        KeyDownAction::SelectPane
    );
    assert_eq!(
        state.key_down(&held("t", meta), false, || false),
        KeyDownAction::Browser
    );
    // A key the pane owns but cannot encode is still kept from the page.
    assert_eq!(
        state.key_down(&TerminalKeyEvent::new("F13"), false, || false),
        KeyDownAction::Consume
    );
}
