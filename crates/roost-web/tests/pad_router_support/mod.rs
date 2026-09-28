//! A hand-rolled document and shell for the controller router: the router
//! reads only the facts `PadDom` / `PadSurfaces` expose, so each fake records
//! what it was asked to do and answers from plain fields.

#![allow(dead_code)]

use std::cell::Cell;
use std::rc::Rc;

use roost_web::input_nav::pad_surfaces::{
    KeypadFocusCancel, PadDom, PadFocus, PadFolderCycle, PadPaneTarget, PadShellAction,
    PadSurfaceState, PadSurfaces, SyntheticKey,
};

/// The focused element and what the router did to the document.
#[derive(Debug)]
pub struct FakeDom {
    pub focus: PadFocus,
    /// Whether dispatched keys go unclaimed (not default-prevented).
    pub keys_unclaimed: bool,
    /// Whether the last key target is the PTY textarea.
    pub key_target_is_terminal_input: bool,
    /// Whether the focused `.wterm` still has range to scroll.
    pub focused_box_can_scroll: bool,
    pub keys: Vec<SyntheticKey>,
    pub clicks: u32,
    pub left_terminal_input: u32,
    pub focused_box_scrolls: Vec<f64>,
    pub pane_scrolls: Vec<(String, f64)>,
    pub terminal_focuses: Vec<Option<String>>,
    pub context_menus: u32,
    pub keypad_focus_starts: u32,
    pub keypad_focus_cancels: Rc<Cell<u32>>,
}

impl Default for FakeDom {
    fn default() -> Self {
        Self {
            focus: PadFocus {
                present: true,
                ..PadFocus::default()
            },
            keys_unclaimed: true,
            key_target_is_terminal_input: false,
            focused_box_can_scroll: false,
            keys: Vec::new(),
            clicks: 0,
            left_terminal_input: 0,
            focused_box_scrolls: Vec::new(),
            pane_scrolls: Vec::new(),
            terminal_focuses: Vec::new(),
            context_menus: 0,
            keypad_focus_starts: 0,
            keypad_focus_cancels: Rc::new(Cell::new(0)),
        }
    }
}

impl FakeDom {
    /// Focus on a terminal scroll box.
    pub fn on_terminal_box() -> Self {
        let mut dom = Self::default();
        dom.focus.terminal_box = true;
        dom
    }

    /// Focus on a key inside the key pad.
    pub fn on_keypad_key() -> Self {
        let mut dom = Self::default();
        dom.focus.in_keypad = true;
        dom
    }
}

impl PadDom for FakeDom {
    fn focus(&self) -> PadFocus {
        self.focus.clone()
    }

    fn press_key(&mut self, key: SyntheticKey) -> bool {
        self.keys.push(key);
        self.keys_unclaimed
    }

    fn click_key_target(&mut self) {
        self.clicks += 1;
    }

    fn leave_terminal_input(&mut self) -> bool {
        if self.key_target_is_terminal_input {
            self.left_terminal_input += 1;
        }
        self.key_target_is_terminal_input
    }

    fn scroll_focused_terminal_box(&mut self, delta_px: f64) -> bool {
        if self.focus.terminal_box && self.focused_box_can_scroll {
            self.focused_box_scrolls.push(delta_px);
            return true;
        }
        false
    }

    fn scroll_pane_terminal_box(&mut self, pane_id: &str, delta_px: f64) {
        self.pane_scrolls.push((pane_id.to_string(), delta_px));
    }

    fn focus_pane_terminal_box(&mut self, pane_id: Option<&str>) {
        self.terminal_focuses.push(pane_id.map(str::to_string));
    }

    fn open_focused_context_menu(&mut self) {
        self.context_menus += 1;
    }

    fn start_keypad_focus(&mut self) -> KeypadFocusCancel {
        self.keypad_focus_starts += 1;
        let cancels = Rc::clone(&self.keypad_focus_cancels);
        Box::new(move || cancels.set(cancels.get() + 1))
    }
}

/// The shell: overlays, key pad and sidebar flags it flips synchronously, and
/// every action it was asked to execute.
#[derive(Debug, Default)]
pub struct FakeSurfaces {
    pub state: PadSurfaceState,
    pub target: Option<PadPaneTarget>,
    pub folders: Option<PadFolderCycle>,
    pub executed: Vec<PadShellAction>,
}

impl FakeSurfaces {
    /// How many times `action` was executed.
    pub fn count(&self, action: &PadShellAction) -> usize {
        self.executed
            .iter()
            .filter(|executed| *executed == action)
            .count()
    }
}

impl PadSurfaces for FakeSurfaces {
    fn state(&self) -> PadSurfaceState {
        self.state
    }

    fn target_pane(&self, _focused_pane_id: Option<&str>) -> Option<PadPaneTarget> {
        self.target.clone()
    }

    fn folder_cycle(&self) -> Option<PadFolderCycle> {
        self.folders.clone()
    }

    fn execute(&mut self, action: PadShellAction) {
        match action {
            PadShellAction::OpenPalette => self.state.palette_open = true,
            PadShellAction::ClosePalette => self.state.palette_open = false,
            PadShellAction::OpenControllerMap => self.state.controller_map_open = true,
            PadShellAction::CloseControllerMap => self.state.controller_map_open = false,
            PadShellAction::ToggleKeypad => self.state.keypad_open = !self.state.keypad_open,
            PadShellAction::CloseKeypad => self.state.keypad_open = false,
            PadShellAction::CloseSidebar => self.state.sidebar_open = false,
            _ => {}
        }
        self.executed.push(action);
    }
}
