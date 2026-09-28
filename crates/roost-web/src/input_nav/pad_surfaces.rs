//! The controller router's two seams: [`PadDom`] (the focused element and the
//! synthetic keys, implemented over `document` by `pad_dom`) and
//! [`PadSurfaces`] (app state and the [`PadShellAction`]s the SHELL executes:
//! palette, controller map, key pad, sidebar, dictation, deck, navigation).
//! Types only; the routing rules are `pad_router`. Split out of the port of
//! `apps/web/src/lib/padActions.ts` so the rules stay native-testable.

/// A key the router dispatches as one untrusted `keydown` on the focused
/// element, so every roving surface that already owns arrows/Enter/Escape
/// keeps owning them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticKey {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Enter,
    Escape,
}

impl SyntheticKey {
    /// `KeyboardEvent.key`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::ArrowLeft => "ArrowLeft",
            Self::ArrowRight => "ArrowRight",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
        }
    }
}

/// `document.activeElement`, as far as the router cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadFocus {
    /// Whether any element holds focus (`<body>` counts).
    pub present: bool,
    /// The element is a `.wterm` terminal scroll box.
    pub terminal_box: bool,
    /// The element is inside the terminal key pad (`.term-nav`).
    pub in_keypad: bool,
    /// The element is inside a `[role="menu"]` or `[role="dialog"]`.
    pub in_menu_or_dialog: bool,
    /// The `data-pane-id` of the pane holding focus, if any.
    pub pane_id: Option<String>,
}

/// Cancels a superseded first-key focus retry.
pub type KeypadFocusCancel = Box<dyn FnOnce()>;

/// The document half: focus facts and the only two things an untrusted key
/// cannot do (native click activation, native scrolling), done explicitly.
pub trait PadDom {
    /// What holds focus right now.
    fn focus(&self) -> PadFocus;
    /// Dispatch `key` on the focused element (or `<body>`), remembering that
    /// element as the key target. Returns whether nobody claimed it (it was not
    /// default-prevented) — how the router asks "did anything own this?".
    fn press_key(&mut self, key: SyntheticKey) -> bool;
    /// Click the last key target: an untrusted ⏎ never activates a native button.
    fn click_key_target(&mut self);
    /// If the last key target is the PTY textarea, blur it and focus its
    /// `.wterm`: the textarea consumes every arrow, so a pad that cannot leave
    /// it is stuck there for the pane's life. Returns whether it did.
    fn leave_terminal_input(&mut self) -> bool;
    /// Scroll the focused `.wterm` by `delta_px`; false when it is already
    /// clamped at that edge (or focus is not on one).
    fn scroll_focused_terminal_box(&mut self, delta_px: f64) -> bool;
    /// Scroll `pane_id`'s `.wterm` by `delta_px`, clamped.
    fn scroll_pane_terminal_box(&mut self, pane_id: &str, delta_px: f64);
    /// Focus `pane_id`'s `.wterm`, else the only one painted.
    fn focus_pane_terminal_box(&mut self, pane_id: Option<&str>);
    /// Dispatch a `contextmenu` at the focused element's centre.
    fn open_focused_context_menu(&mut self);
    /// Start focusing the key pad's first key once its portal paints.
    fn start_keypad_focus(&mut self) -> KeypadFocusCancel;
}

/// The dictation composer, as far as the router cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadDictation {
    /// A dictation is owned right now.
    pub dictating: bool,
    /// A composer that owns the mic is mounted for the focused pane.
    pub controls_mounted: bool,
    /// Whether that composer could start the mic without a user gesture. A
    /// Gamepad press carries no user activation, so a browser that gates
    /// `getUserMedia` on a gesture denies a pad-started mic without prompting.
    pub can_start_without_gesture: bool,
}

/// App state the router reads before each intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadSurfaceState {
    /// The terminal key pad (`store::terminal_nav_pad`).
    pub keypad_open: bool,
    /// The controller map (Start guide).
    pub controller_map_open: bool,
    /// The command palette.
    pub palette_open: bool,
    /// The help overlay.
    pub help_open: bool,
    /// The mobile drawer (`store::ui::sidebar_open`).
    pub sidebar_open: bool,
    /// The dictation composer.
    pub dictation: PadDictation,
}

/// The pane the deck intents address: the one holding DOM focus, else the
/// layout's focused pane — resolved by the shell against the active session's
/// layout, `None` when no open session is routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadPaneTarget {
    /// The target pane.
    pub pane_id: String,
    /// Its tabs, in strip order.
    pub tabs: Vec<String>,
    /// Its selected tab.
    pub selected_tab: String,
    /// Every pane in the layout, in leaf order.
    pub layout_pane_ids: Vec<String>,
}

/// The folder list the next-folder press cycles through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadFolderCycle {
    /// Folders ordered by latest activity, newest first.
    pub folders: Vec<crate::input_nav::pad_folders::FolderLead>,
    /// The folder of the session the current route shows, if any.
    pub current_folder_key: Option<String>,
}

/// The intents that need another surface. Emitted by `pad_router`, executed
/// SYNCHRONOUSLY by the SHELL's [`PadSurfaces::execute`], because the router
/// re-reads [`PadSurfaces::state`] right after some of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadShellAction {
    OpenPalette,
    ClosePalette,
    OpenControllerMap,
    CloseControllerMap,
    /// `store::terminal_nav_pad::toggle_terminal_nav_pad`.
    ToggleKeypad,
    /// `store::terminal_nav_pad::close_terminal_nav_pad` — the ONE close path,
    /// which disarms the sheet's latched modifiers.
    CloseKeypad,
    /// `store::ui::close_sidebar`.
    CloseSidebar,
    /// Start, stop-and-send, or commit the dictation (`voiceControls.toggle`).
    ToggleDictation,
    /// Throw the owned dictation away (`voiceControls.discard`).
    DiscardDictation,
    /// Select `tab` in the target pane (deck `selectTabOp`).
    SelectTab {
        tab: String,
    },
    /// Focus `pane_id` (deck `focusPaneOp`).
    FocusPane {
        pane_id: String,
    },
    /// Navigate to `/s/<session_id>`.
    OpenSession {
        session_id: String,
    },
    /// Raise a warning toast.
    Warn {
        message: &'static str,
    },
}

/// The app half, implemented by the SHELL over the store and its overlays.
pub trait PadSurfaces {
    /// Current overlay / key pad / dictation state.
    fn state(&self) -> PadSurfaceState;
    /// The deck target for `focused_pane_id` (the DOM focus's pane, if any).
    fn target_pane(&self, focused_pane_id: Option<&str>) -> Option<PadPaneTarget>;
    /// The folder cycle, or `None` before the router is wired.
    fn folder_cycle(&self) -> Option<PadFolderCycle>;
    /// Perform `action` now.
    fn execute(&mut self, action: PadShellAction);
}
