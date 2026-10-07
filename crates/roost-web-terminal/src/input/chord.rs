//! One key event, described without a DOM: the key itself, the four modifier
//! levels, the AltGraph report, and whether an IME composition owns it.
//!
//! This is the only shape the encoder sees. The `dom` adapter builds one from
//! a `web_sys::KeyboardEvent`, and the touch key pad builds one directly, so
//! application-mode and platform behaviour are decidable in a test and the two
//! input paths cannot drift into different encodings. Ports v2's
//! `TerminalKeyEvent` and `isAltGraphKey` from `apps/web/src/client/input/terminalInput.ts`.

/// Which key an event names, in the vocabulary the encoder dispatches on.
///
/// The three `BrowserOwned` spellings are what a browser reports for a key it
/// is still resolving — a dead key awaiting composition, an IME key, a key the
/// platform could not identify — and all three belong to the text services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// One printable code point, astral characters included.
    Printable(char),
    /// A named key with no printable form.
    Named(NamedKey),
    /// Dead, Process, or Unidentified: the browser's text services own it.
    BrowserOwned,
}

/// The named keys the encoder gives an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
    Enter,
    Backspace,
    Tab,
    Escape,
    /// A kitty functional key code (57358–57454).
    Functional(u16),
}

impl NamedKey {
    /// The name a `KeyboardEvent` reports for this key.
    pub const fn dom_key(self) -> &'static str {
        match self {
            Self::ArrowUp => "ArrowUp",
            Self::ArrowDown => "ArrowDown",
            Self::ArrowRight => "ArrowRight",
            Self::ArrowLeft => "ArrowLeft",
            Self::Home => "Home",
            Self::End => "End",
            Self::Insert => "Insert",
            Self::Delete => "Delete",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
            Self::Function(1) => "F1",
            Self::Function(2) => "F2",
            Self::Function(3) => "F3",
            Self::Function(4) => "F4",
            Self::Function(5) => "F5",
            Self::Function(6) => "F6",
            Self::Function(7) => "F7",
            Self::Function(8) => "F8",
            Self::Function(9) => "F9",
            Self::Function(10) => "F10",
            Self::Function(11) => "F11",
            Self::Function(12) => "F12",
            Self::Function(13) => "F13",
            Self::Function(14) => "F14",
            Self::Function(15) => "F15",
            Self::Function(16) => "F16",
            Self::Function(17) => "F17",
            Self::Function(18) => "F18",
            Self::Function(19) => "F19",
            Self::Function(20) => "F20",
            Self::Function(21) => "F21",
            Self::Function(22) => "F22",
            Self::Function(23) => "F23",
            Self::Function(24) => "F24",
            Self::Function(25) => "F25",
            Self::Function(26) => "F26",
            Self::Function(27) => "F27",
            Self::Function(28) => "F28",
            Self::Function(29) => "F29",
            Self::Function(30) => "F30",
            Self::Function(31) => "F31",
            Self::Function(32) => "F32",
            Self::Function(33) => "F33",
            Self::Function(34) => "F34",
            Self::Function(35) => "F35",
            Self::Enter => "Enter",
            Self::Backspace => "Backspace",
            Self::Tab => "Tab",
            Self::Escape => "Escape",
            Self::Function(_) | Self::Functional(_) => "",
        }
    }

    /// The name for `key`, or `None` for an unrecognized key.
    pub fn from_dom_key(key: &str) -> Option<Self> {
        let named = match key {
            "ArrowUp" => Self::ArrowUp,
            "ArrowDown" => Self::ArrowDown,
            "ArrowRight" => Self::ArrowRight,
            "ArrowLeft" => Self::ArrowLeft,
            "Home" => Self::Home,
            "End" => Self::End,
            "Insert" => Self::Insert,
            "Delete" => Self::Delete,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "Enter" => Self::Enter,
            "Backspace" => Self::Backspace,
            "Tab" => Self::Tab,
            "Escape" => Self::Escape,
            _ if key.strip_prefix('F').is_some_and(|number| {
                number.parse::<u8>().is_ok_and(|number| (1..=35).contains(&number))
            }) => Self::Function(key[1..].parse().ok()?),
            "CapsLock" => Self::Functional(57358),
            "ScrollLock" => Self::Functional(57359),
            "NumLock" => Self::Functional(57360),
            "PrintScreen" => Self::Functional(57361),
            "Pause" => Self::Functional(57362),
            "ContextMenu" => Self::Functional(57363),
            "Numpad0" => Self::Functional(57399),
            "Numpad1" => Self::Functional(57400),
            "Numpad2" => Self::Functional(57401),
            "Numpad3" => Self::Functional(57402),
            "Numpad4" => Self::Functional(57403),
            "Numpad5" => Self::Functional(57404),
            "Numpad6" => Self::Functional(57405),
            "Numpad7" => Self::Functional(57406),
            "Numpad8" => Self::Functional(57407),
            "Numpad9" => Self::Functional(57408),
            "NumpadDecimal" => Self::Functional(57409),
            "NumpadDivide" => Self::Functional(57410),
            "NumpadMultiply" => Self::Functional(57411),
            "NumpadSubtract" => Self::Functional(57412),
            "NumpadAdd" => Self::Functional(57413),
            "NumpadEnter" => Self::Functional(57414),
            "NumpadEqual" => Self::Functional(57415),
            "NumpadComma" => Self::Functional(57416),
            "MediaPlay" => Self::Functional(57428),
            "MediaPause" => Self::Functional(57429),
            "MediaPlayPause" => Self::Functional(57430),
            "MediaReverse" => Self::Functional(57431),
            "MediaStop" => Self::Functional(57432),
            "MediaFastForward" => Self::Functional(57433),
            "MediaRewind" => Self::Functional(57434),
            "MediaTrackNext" => Self::Functional(57435),
            "MediaTrackPrevious" => Self::Functional(57436),
            "MediaRecord" => Self::Functional(57437),
            "AudioVolumeDown" => Self::Functional(57438),
            "AudioVolumeUp" => Self::Functional(57439),
            "AudioVolumeMute" => Self::Functional(57440),
            "Shift" => Self::Functional(57441),
            "Control" => Self::Functional(57442),
            "Alt" => Self::Functional(57443),
            "Meta" => Self::Functional(57444),
            _ => return None,
        };
        Some(named)
    }
}

impl KeyKind {
    /// Classify the `key` string a `KeyboardEvent` reports.
    pub fn from_dom_key(key: &str) -> Self {
        if let Some(named) = NamedKey::from_dom_key(key) {
            return Self::Named(named);
        }
        if matches!(key, "Dead" | "Process" | "Unidentified") {
            return Self::BrowserOwned;
        }
        let mut characters = key.chars();
        match (characters.next(), characters.next()) {
            (Some(character), None) => Self::Printable(character),
            _ => Self::BrowserOwned,
        }
    }
}


/// The eight modifier levels represented by kitty's modifier field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub super_key: bool,
    pub hyper: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false, alt: false, ctrl: false, meta: false,
        super_key: false, hyper: false, caps_lock: false, num_lock: false,
    };
    pub const SHIFT: Self = Self { shift: true, ..Self::NONE };
}

/// Kitty key-event type, carried only when the terminal requested event types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

/// Optional shifted and standard-layout key alternatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlternateKeys {
    pub shifted: Option<char>,
    pub base_layout: Option<char>,
    pub unshifted: Option<char>,
}

/// One key event, described without a DOM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    /// Which key was pressed.
    pub kind: KeyKind,
    /// The modifier levels at press time.
    pub modifiers: Modifiers,
    /// Whether the platform reports AltGraph for this event, which is how a
    /// Windows layout says a character is text rather than a Ctrl binding.
    pub alt_graph: bool,
    /// Whether an IME composition owns this event.
    pub is_composing: bool,
}

impl KeyChord {
    /// A plain printable key with no modifiers.
    pub fn printable(character: char) -> Self {
        Self {
            kind: KeyKind::Printable(character),
            modifiers: Modifiers::NONE,
            alt_graph: false,
            is_composing: false,
        }
    }

    /// A named key with the given modifiers.
    pub fn named(key: NamedKey, modifiers: Modifiers) -> Self {
        Self {
            kind: KeyKind::Named(key),
            modifiers,
            alt_graph: false,
            is_composing: false,
        }
    }

    /// This chord with `modifiers` held at press time.
    pub fn with_modifiers(mut self, modifiers: Modifiers) -> Self {
        self.modifiers = modifiers;
        self
    }

    /// This chord with `alt_graph` set, which is how a Windows layout reports
    /// AltGraph.
    pub fn with_alt_graph(mut self, alt_graph: bool) -> Self {
        self.alt_graph = alt_graph;
        self
    }

    /// This chord marked as owned by an IME composition.
    pub fn composing(mut self) -> Self {
        self.is_composing = true;
        self
    }

    /// Whether this is an AltGraph key.
    ///
    /// Windows reports AltGraph either explicitly through the modifier state
    /// or implicitly as Ctrl+Alt over a printable character, and both are text
    /// input. The Ctrl+Alt form is the one that matters: read as a Ctrl
    /// binding it becomes a control byte plus an ESC prefix, so one character
    /// of a word reaches the shell as three.
    pub fn is_alt_graph(&self) -> bool {
        self.alt_graph
            || (self.modifiers.ctrl
                && self.modifiers.alt
                && !self.modifiers.meta
                && matches!(self.kind, KeyKind::Printable(_)))
    }

    /// Whether this key is one printable code point.
    pub fn is_printable(&self) -> bool {
        matches!(self.kind, KeyKind::Printable(_))
    }

    /// The bytes a PTY receives for this key, or `None` when the pane does not
    /// own the key. The worker's DECCKM cursor mode decides the arrow and
    /// home/end encoding.
    pub fn to_bytes(&self, cursor_keys_application: bool) -> Option<Vec<u8>> {
        super::keys::terminal_key_sequence(self, cursor_keys_application, 0).map(String::into_bytes)
    }
}
