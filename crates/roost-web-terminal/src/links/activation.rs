//! Which gesture opens a terminal link, and the words that name it: shared by the
//! link attachment and the mouse-forwarding adapter, which withholds a link press
//! from DECSET reporting. Pure; the caller passes the platform's key. Ports the
//! gesture and hint/title text of `apps/web/src/renderer/terminal-links.ts` and
//! `terminal-links.dom.ts`, and the press rule of `terminalMouseForwarding.ts`.

/// The physical modifier key a platform opens a terminal link with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkModifierKey {
    /// Everywhere but macOS: Ctrl-click.
    Control,
    /// macOS: Command-click.
    Meta,
}

impl LinkModifierKey {
    /// The key for a platform, which is the one thing that decides it.
    pub const fn for_platform(is_macos: bool) -> Self {
        if is_macos { Self::Meta } else { Self::Control }
    }

    /// `KeyboardEvent.key` for this modifier.
    pub const fn event_key(self) -> &'static str {
        match self {
            Self::Control => "Control",
            Self::Meta => "Meta",
        }
    }

    /// The key as the hover hint names it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Control => "Ctrl",
            Self::Meta => "⌘",
        }
    }
}

/// A mouse event as the link predicates see it. Every field is a plain level,
/// so an event carrying no modifier field reads as "not held", never as an
/// absent third value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkActivationGesture {
    /// DOM `MouseEvent.button`; only 0 activates.
    pub button: i16,
    /// Ctrl held.
    pub ctrl: bool,
    /// Command held.
    pub meta: bool,
    /// Shift, which selects text instead of opening anything.
    pub shift: bool,
    /// Alt, which selects text instead of opening anything.
    pub alt: bool,
}

/// Whether a click activates a terminal link rather than selecting text.
/// Physical modifiers come from the event; compact arming is pane-local state
/// and wins outright. Holding BOTH physical keys is never the gesture.
pub fn is_link_activation_gesture(
    gesture: &LinkActivationGesture,
    activation_armed: bool,
    modifier_key: LinkModifierKey,
) -> bool {
    if gesture.button != 0 || gesture.shift || gesture.alt {
        return false;
    }
    if activation_armed {
        return true;
    }
    match modifier_key {
        LinkModifierKey::Meta => gesture.meta && !gesture.ctrl,
        LinkModifierKey::Control => gesture.ctrl && !gesture.meta,
    }
}

/// Whether the platform's link modifier is held. A LEVEL, and total: every
/// pointer event re-derives the armed state from it in both directions.
pub fn is_link_modifier_held(
    gesture: &LinkActivationGesture,
    modifier_key: LinkModifierKey,
) -> bool {
    match modifier_key {
        LinkModifierKey::Meta => gesture.meta,
        LinkModifierKey::Control => gesture.ctrl,
    }
}

/// The floating hint text: the key that opens the link, then the anchor's
/// `data-hint` without the `Open ` prefix a file hint carries.
pub fn link_hint_text(modifier_key: LinkModifierKey, hint: &str) -> String {
    let hint = hint.strip_prefix("Open ").unwrap_or(hint);
    format!("{}-click to open · {hint}", modifier_key.label())
}

/// The `title` an authored anchor carries, with the key spelled out.
pub fn link_title(modifier_key: LinkModifierKey, display: &str) -> String {
    let key = match modifier_key {
        LinkModifierKey::Control => "Control",
        LinkModifierKey::Meta => "Command",
    };
    format!("{key}-click to open {display}")
}

/// Why a press was not forwarded to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressWithheld {
    /// It is the exact local link gesture, which the terminal itself owns.
    LinkActivation,
    /// The middle button, reserved for the deck's bring-to-front toggle.
    DeckMiddleButton,
}

/// Whether a press is withheld from DECSET mouse reporting, and why. The exact
/// local link gesture wins; a bare anchor click falls through so a mouse-aware
/// TUI keeps its press inside a link.
pub fn withhold_press(
    over_terminal_link: bool,
    gesture: &LinkActivationGesture,
    activation_armed: bool,
    modifier_key: LinkModifierKey,
    button_is_middle: bool,
) -> Option<PressWithheld> {
    if over_terminal_link && is_link_activation_gesture(gesture, activation_armed, modifier_key) {
        return Some(PressWithheld::LinkActivation);
    }
    if button_is_middle {
        return Some(PressWithheld::DeckMiddleButton);
    }
    None
}
