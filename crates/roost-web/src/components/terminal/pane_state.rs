//! The two value types one terminal pane passes between its Dioxus component
//! and its imperative mount: the props-derived flags the mount gates on, and
//! the signals the mount writes so the component re-renders only when what it
//! shows changed. Read by `cell_terminal` and the wasm `pane_mount`. Ports the
//! `viewActive` memo and the pane signals of
//! `apps/web/src/components/terminal/CellTerminal.tsx`.

use dioxus::prelude::*;
use roost_web_terminal::terminal_presentation::TerminalPresentationState;

use super::pane_status::TerminalStartupNotice;
use super::terminal_find_bar::FindBarState;

/// The props the mount gates every transition on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneFlags {
    /// In the current tiling layout.
    pub in_layout: bool,
    /// Owns the keyboard.
    pub focused: bool,
    /// The floated pane's selected tab.
    pub spotlit: bool,
    /// No non-terminal route overlays the deck.
    pub surface_visible: bool,
    /// No other pane's spotlight scrim covers this one.
    pub surface_active: bool,
    /// The session is still an optimistic spawn.
    pub pending: bool,
}

impl PaneFlags {
    /// The one foreground gate: publication, focus and global listeners all
    /// read this and nothing else.
    pub const fn view_active(&self) -> bool {
        self.in_layout && self.surface_visible && self.surface_active
    }
}

/// What the component renders, written by the mount.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneUi {
    /// The stream indicator.
    pub presentation: Signal<TerminalPresentationState>,
    /// The startup card.
    pub notice: Signal<Option<TerminalStartupNotice>>,
    /// The offline notice.
    pub offline: Signal<bool>,
    /// The painted frame is on the alternate screen.
    pub alt_screen: Signal<bool>,
    /// Gestures belong to the application (`touch-action: none`).
    pub gestures_forwarded: Signal<bool>,
    /// A multiline paste awaiting confirmation.
    pub pending_paste: Signal<Option<String>>,
    /// The find bar is open.
    pub find_open: Signal<bool>,
    /// What the find bar renders, `None` while it is closed.
    pub find_bar: Signal<Option<FindBarState>>,
    /// The on-screen Ctrl latch.
    pub ctrl_armed: Signal<bool>,
    /// The on-screen Alt link-activation latch.
    pub link_armed: Signal<bool>,
    /// The confirmed carrier's `data-terminal-transport` spelling.
    pub transport: Signal<Option<&'static str>>,
    /// The reader is parked in history, away from the live tail: the
    /// jump-to-bottom button shows.
    pub scrolled_back: Signal<bool>,
    /// The share of the grid below the cursor row, in per mille: how much of
    /// the display a growing composer or an open tray may cover before the
    /// prompt has to be lifted out of its way.
    pub free_below_permille: Signal<u16>,
}

impl PaneUi {
    /// Fresh signals, owned by the calling component's scope.
    pub fn use_pane_ui() -> Self {
        Self {
            presentation: use_signal(TerminalPresentationState::default),
            notice: use_signal(|| None),
            offline: use_signal(|| false),
            alt_screen: use_signal(|| false),
            gestures_forwarded: use_signal(|| false),
            pending_paste: use_signal(|| None),
            find_open: use_signal(|| false),
            find_bar: use_signal(|| None),
            ctrl_armed: use_signal(|| false),
            link_armed: use_signal(|| false),
            transport: use_signal(|| None),
            scrolled_back: use_signal(|| false),
            free_below_permille: use_signal(|| 0),
        }
    }
}

/// Write a signal only when the value changed, so an unchanged pump revision
/// never re-renders the pane.
pub fn set_if_changed<T: PartialEq + 'static>(mut signal: Signal<T>, value: T) {
    if *signal.peek() != value {
        signal.set(value);
    }
}

/// The stream indicator's state token and label, or `None` while idle.
pub fn presentation_indicator(
    state: TerminalPresentationState,
) -> Option<(&'static str, &'static str)> {
    match state {
        TerminalPresentationState::Receiving => Some(("receiving", "Receiving terminal frames")),
        TerminalPresentationState::CatchingUp => Some(("catching_up", "Screen catching up")),
        TerminalPresentationState::Detached => Some(("detached", "No live terminal stream")),
        TerminalPresentationState::Idle => None,
    }
}
