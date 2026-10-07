//! Controller intents → the app's EXISTING focus/menu/deck machinery.
//! Directional and activate/back intents become one untrusted synthetic key on
//! the focused element, so every roving surface that already owns
//! arrows/Enter/Escape keeps owning them; only native click activation and
//! native scrolling are compensated explicitly. Everything else is a
//! [`PadShellAction`] the SHELL executes. Also owns the transient legend state.
//! Called by the App's gamepad callback (`install_gamepad_source`); depends on
//! `pad_surfaces`, `pad_bindings`, `pad_folders`, `roost_web_terminal::reader_scroll`.
//! Ported from `apps/web/src/lib/padActions.ts`.

use roost_web_terminal::reader_scroll::PAD_SCROLL_STEP_PX;

use crate::input_nav::pad_bindings::PadAction;
use crate::input_nav::pad_folders::next_folder_session_id;
use crate::input_nav::pad_hints::PadHintContext;
use crate::input_nav::pad_surfaces::{
    KeypadFocusCancel, PadDom, PadFocus, PadShellAction, PadSurfaceState, PadSurfaces, SyntheticKey,
};

/// The legend hides this long after the last controller input.
pub const PAD_HINT_IDLE_MS: f64 = 4000.0;

/// What a mic press with no grant says, once.
pub const MIC_NEEDS_GESTURE_WARNING: &str =
    "Tap the mic once to allow it — the controller can start it after that.";

/// The transient legend: whether it shows, and which bindings it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadHints {
    pub visible: bool,
    pub context: PadHintContext,
}

/// The router's own state: the legend, the live key-pad focus retry, and the
/// "mic needs a gesture" latch.
#[derive(Default)]
pub struct PadActionRouter {
    hints: PadHints,
    hints_hide_at_ms: f64,
    // The key-pad focus retry outlives the press that started it, so a second
    // open must cancel the first: two live retries would fight over focus.
    keypad_focus_cancel: Option<KeypadFocusCancel>,
    // Latched so a mashed stick click does not stack copies of the same advice.
    mic_gesture_required: bool,
}

impl std::fmt::Debug for PadActionRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PadActionRouter")
            .field("hints", &self.hints)
            .field("hints_hide_at_ms", &self.hints_hide_at_ms)
            .field("keypad_focus_live", &self.keypad_focus_cancel.is_some())
            .field("mic_gesture_required", &self.mic_gesture_required)
            .finish()
    }
}

impl PadActionRouter {
    /// No legend, no retry, no latch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The legend, for the hint bar.
    pub fn hints(&self) -> PadHints {
        self.hints
    }

    /// When the legend hides unless another intent arrives first.
    pub fn hints_hide_at_ms(&self) -> f64 {
        self.hints_hide_at_ms
    }

    /// Hide the legend once its idle window has passed. Returns whether it hid.
    /// Deadline-based, so a superseded timer firing early changes nothing.
    pub fn expire_hints(&mut self, now_ms: f64) -> bool {
        if !self.hints.visible || now_ms < self.hints_hide_at_ms {
            return false;
        }
        self.hints.visible = false;
        tracing::debug!(target: "input_nav", "pad hints hidden");
        true
    }

    /// Run one poll's intents. Inert unless controller mode is active.
    pub fn run_pad_actions(
        &mut self,
        actions: &[PadAction],
        pad_mode_active: bool,
        now_ms: f64,
        dom: &mut dyn PadDom,
        surfaces: &mut dyn PadSurfaces,
    ) {
        if !pad_mode_active {
            return;
        }
        for action in actions.iter().copied() {
            // The controller map is the read-the-buttons surface: exploring the
            // pad must not fire what the diagram documents, and the diagram IS
            // the legend while it is up. Start toggles it shut and B closes it;
            // nothing else gets through.
            let map_open = surfaces.state().controller_map_open;
            if map_open && action != PadAction::ControllerMap && action != PadAction::Back {
                continue;
            }
            if !map_open {
                self.hints.context = hint_context(&surfaces.state(), &dom.focus());
                self.hints.visible = true;
                self.hints_hide_at_ms = now_ms + PAD_HINT_IDLE_MS;
            }
            self.dispatch(action, dom, surfaces);
            tracing::info!(
                target: "input_nav",
                action = action.as_str(),
                context = self.hints.context.as_str(),
                "pad.action"
            );
        }
    }

    /// A TV remote's OK-on-the-terminal or Back key, which mean what the pad's
    /// A and B mean: the same chain, so one decision serves both inputs. Not
    /// gated on controller mode and raises no legend — the remote is the TV's
    /// own input, and the legend names pad buttons a remote does not have.
    pub fn run_remote_action(
        &mut self,
        action: PadAction,
        dom: &mut dyn PadDom,
        surfaces: &mut dyn PadSurfaces,
    ) {
        self.dispatch(action, dom, surfaces);
        tracing::info!(target: "input_nav", action = action.as_str(), "remote.action");
    }

    fn dispatch(
        &mut self,
        action: PadAction,
        dom: &mut dyn PadDom,
        surfaces: &mut dyn PadSurfaces,
    ) {
        match action {
            PadAction::MoveUp | PadAction::MoveDown => {
                // The focused scroll box owns ↑/↓ until clamped, then the
                // direction becomes focus travel — the scroll write's result IS
                // the clamp.
                let (delta, key) = if action == PadAction::MoveUp {
                    (-PAD_SCROLL_STEP_PX, SyntheticKey::ArrowUp)
                } else {
                    (PAD_SCROLL_STEP_PX, SyntheticKey::ArrowDown)
                };
                if dom.focus().terminal_box && dom.scroll_focused_terminal_box(delta) {
                    return;
                }
                dom.press_key(key);
            }
            PadAction::MoveLeft => {
                dom.press_key(SyntheticKey::ArrowLeft);
            }
            PadAction::MoveRight => {
                dom.press_key(SyntheticKey::ArrowRight);
            }
            PadAction::ScrollUp | PadAction::ScrollDown => {
                let delta = if action == PadAction::ScrollUp {
                    -PAD_SCROLL_STEP_PX
                } else {
                    PAD_SCROLL_STEP_PX
                };
                // Scroll-only: the right stick never falls through to focus travel.
                if let Some(target) = surfaces.target_pane(dom.focus().pane_id.as_deref()) {
                    dom.scroll_pane_terminal_box(&target.pane_id, delta);
                }
            }
            PadAction::Activate => self.activate(dom, surfaces),
            PadAction::Back => self.back(dom, surfaces),
            PadAction::Palette => surfaces.execute(if surfaces.state().palette_open {
                PadShellAction::ClosePalette
            } else {
                PadShellAction::OpenPalette
            }),
            PadAction::ContextMenu => {
                if dom.focus().present {
                    dom.open_focused_context_menu();
                }
            }
            PadAction::TabPrev => step_tab(-1, dom, surfaces),
            PadAction::TabNext => step_tab(1, dom, surfaces),
            PadAction::PanePrev => step_pane(-1, dom, surfaces),
            PadAction::PaneNext => step_pane(1, dom, surfaces),
            PadAction::Keypad => self.toggle_keypad(dom, surfaces),
            PadAction::ControllerMap => surfaces.execute(if surfaces.state().controller_map_open {
                PadShellAction::CloseControllerMap
            } else {
                PadShellAction::OpenControllerMap
            }),
            PadAction::MicToggle => self.toggle_dictation(surfaces),
            PadAction::FolderNext => {
                let Some(cycle) = surfaces.folder_cycle() else {
                    return;
                };
                if let Some(session_id) =
                    next_folder_session_id(&cycle.folders, cycle.current_folder_key.as_deref())
                {
                    let session_id = session_id.to_string();
                    surfaces.execute(PadShellAction::OpenSession { session_id });
                }
            }
        }
    }

    fn activate(&mut self, dom: &mut dyn PadDom, surfaces: &mut dyn PadSurfaces) {
        // While a dictation is owned, A is the commit gesture: the pad has no
        // other way to accept a transcript.
        let dictation = surfaces.state().dictation;
        if dictation.dictating {
            if dictation.controls_mounted {
                surfaces.execute(PadShellAction::ToggleDictation);
            }
            return;
        }
        // On the terminal box the pad has no keyboard, so A opens the one
        // surface that sends raw keys — and lands on a key, or the D-pad would
        // have nothing to travel between. No synthetic key: the box would
        // swallow it into the PTY.
        if dom.focus().terminal_box {
            self.toggle_keypad(dom, surfaces);
            return;
        }
        // An untrusted key never activates a native button; unclaimed ⏎ means
        // nobody owned it, so the click is ours to make.
        if dom.press_key(SyntheticKey::Enter) {
            dom.click_key_target();
        }
    }

    fn back(&mut self, dom: &mut dyn PadDom, surfaces: &mut dyn PadSurfaces) {
        let state = surfaces.state();
        if state.dictation.dictating {
            if state.dictation.controls_mounted {
                surfaces.execute(PadShellAction::DiscardDictation);
            }
            return;
        }
        let focus = dom.focus();
        // The key pad renders in a body portal, so an Escape from inside it would
        // escape past it to whatever owns the document. B leaves the pad
        // explicitly and hands the terminal its focus back.
        if state.keypad_open && focus.in_keypad {
            surfaces.execute(PadShellAction::CloseKeypad);
            let target = surfaces.target_pane(focus.pane_id.as_deref());
            dom.focus_pane_terminal_box(target.as_ref().map(|target| target.pane_id.as_str()));
            return;
        }
        // Kobalte-style dialogs dismiss on an untrusted document keydown without
        // preventDefault, so a dispatched Escape would close the map and let the
        // rest of this chain close the drawer behind it too. One press, one
        // effect: close it here.
        if state.controller_map_open {
            surfaces.execute(PadShellAction::CloseControllerMap);
            return;
        }
        if !dom.press_key(SyntheticKey::Escape) {
            return;
        }
        if dom.leave_terminal_input() {
            return;
        }
        if surfaces.state().sidebar_open {
            surfaces.execute(PadShellAction::CloseSidebar);
        }
    }

    fn toggle_keypad(&mut self, dom: &mut dyn PadDom, surfaces: &mut dyn PadSurfaces) {
        surfaces.execute(PadShellAction::ToggleKeypad);
        if surfaces.state().keypad_open {
            if let Some(cancel) = self.keypad_focus_cancel.take() {
                cancel();
            }
            self.keypad_focus_cancel = Some(dom.start_keypad_focus());
        }
    }

    fn toggle_dictation(&mut self, surfaces: &mut dyn PadSurfaces) {
        let dictation = surfaces.state().dictation;
        // The composer that owns the mic mounts per focused pane, so a dead
        // button here is a real state, not a bug — say which state it was.
        if !dictation.controls_mounted {
            tracing::info!(target: "input_nav", action = "mic-toggle", reason = "no-composer", "pad.action_unavailable");
            return;
        }
        // Stopping never needs permission; only a start does, and the pad cannot
        // supply the user activation some browsers demand for it.
        if !dictation.dictating && !dictation.can_start_without_gesture {
            tracing::info!(target: "input_nav", action = "mic-toggle", reason = "mic-needs-gesture", "pad.action_unavailable");
            if !self.mic_gesture_required {
                self.mic_gesture_required = true;
                surfaces.execute(PadShellAction::Warn {
                    message: MIC_NEEDS_GESTURE_WARNING,
                });
            }
            return;
        }
        self.mic_gesture_required = false;
        surfaces.execute(PadShellAction::ToggleDictation);
    }
}

/// The legend context for what is focused now. The controller map stands the
/// router down and is its own legend, so it never reaches here.
pub fn hint_context(state: &PadSurfaceState, focus: &PadFocus) -> PadHintContext {
    if state.dictation.dictating {
        PadHintContext::Dictation
    } else if focus.in_keypad {
        PadHintContext::Keypad
    } else if state.palette_open || state.help_open {
        PadHintContext::Overlay
    } else if focus.in_menu_or_dialog {
        PadHintContext::Menu
    } else if focus.terminal_box {
        PadHintContext::Terminal
    } else {
        PadHintContext::Default
    }
}

/// The entry `step` away from `current` in a ring of `len`, wrapping at both
/// ends — a console UI cycles rather than dead-ending at an end stop. An absent
/// `current` counts from −1, as `indexOf` does.
pub fn cycle_index(current: Option<usize>, step: isize, len: usize) -> Option<usize> {
    let len_signed = isize::try_from(len).ok().filter(|len| *len > 0)?;
    let from = current
        .and_then(|index| isize::try_from(index).ok())
        .unwrap_or(-1);
    usize::try_from((from + step).rem_euclid(len_signed)).ok()
}

fn step_tab(step: isize, dom: &mut dyn PadDom, surfaces: &mut dyn PadSurfaces) {
    let Some(target) = surfaces.target_pane(dom.focus().pane_id.as_deref()) else {
        return;
    };
    if target.tabs.len() < 2 {
        return;
    }
    let current = target
        .tabs
        .iter()
        .position(|tab| *tab == target.selected_tab);
    if let Some(tab) =
        cycle_index(current, step, target.tabs.len()).and_then(|next| target.tabs.get(next))
    {
        surfaces.execute(PadShellAction::SelectTab { tab: tab.clone() });
    }
}

fn step_pane(step: isize, dom: &mut dyn PadDom, surfaces: &mut dyn PadSurfaces) {
    let Some(target) = surfaces.target_pane(dom.focus().pane_id.as_deref()) else {
        return;
    };
    let panes = &target.layout_pane_ids;
    if panes.len() < 2 {
        return;
    }
    let current = panes.iter().position(|pane| *pane == target.pane_id);
    if let Some(pane_id) = cycle_index(current, step, panes.len()).and_then(|next| panes.get(next))
    {
        surfaces.execute(PadShellAction::FocusPane {
            pane_id: pane_id.clone(),
        });
    }
}
