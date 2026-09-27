//! One key event, described without a DOM: the key itself, the four modifier
//! levels, the AltGraph report, and whether an IME composition owns it.
//!
//! This is the only shape the encoder sees. The `dom` adapter builds one from
//! a `web_sys::KeyboardEvent`, and the touch key pad builds one directly, so
//! application-mode and platform behaviour are decidable in a test and the two
//! input paths cannot drift into different encodings.

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
    /// ArrowUp, ArrowDown, ArrowRight, ArrowLeft, Home, End.
    ArrowUp,
    /// See `ArrowUp`.
    ArrowDown,
    /// See `ArrowUp`.
    ArrowRight,
    /// See `ArrowUp`.
    ArrowLeft,
    /// See `ArrowUp`.
    Home,
    /// See `ArrowUp`.
    End,
    /// Insert, Delete, PageUp, PageDown.
    Insert,
    /// See `Insert`.
    Delete,
    /// See `Insert`.
    PageUp,
    /// See `Insert`.
    PageDown,
    /// F1 through F12. F1–F4 encode as SS3 and F5–F12 as `CSI n ~`, which is
    /// the one place the two encodings disagree about a key, so the number is
    /// carried rather than flattened into a sequence.
    Function(u8),
    /// Enter.
    Enter,
    /// Backspace.
    Backspace,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
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
            Self::Enter => "Enter",
            Self::Backspace => "Backspace",
            Self::Tab => "Tab",
            Self::Escape => "Escape",
            // A function number outside F1-F12 is not a key a browser reports,
            // and a fabricated name would be a lie the encoder then acts on.
            Self::Function(_) => "",
        }
    }

    /// The name for `key`, or `None` for anything the encoder has no sequence
    /// for. `Function` accepts only 1–12; F13 and up are unmapped here exactly
    /// as they are unmapped in the browser, because a PTY receiving a guess
    /// for F13 gets bytes an application will read as something else.
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
            "F1" => Self::Function(1),
            "F2" => Self::Function(2),
            "F3" => Self::Function(3),
            "F4" => Self::Function(4),
            "F5" => Self::Function(5),
            "F6" => Self::Function(6),
            "F7" => Self::Function(7),
            "F8" => Self::Function(8),
            "F9" => Self::Function(9),
            "F10" => Self::Function(10),
            "F11" => Self::Function(11),
            "F12" => Self::Function(12),
            "Enter" => Self::Enter,
            "Backspace" => Self::Backspace,
            "Tab" => Self::Tab,
            "Escape" => Self::Escape,
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
        // A one-code-point key is text. Anything longer is a key this encoder
        // has no sequence for, and sending nothing is the only safe answer:
        // a multi-character `key` is a named key this list does not cover.
        let mut characters = key.chars();
        match (characters.next(), characters.next()) {
            (Some(character), None) => Self::Printable(character),
            _ => Self::BrowserOwned,
        }
    }
}

/// The four modifier levels an event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Alt, or Option on macOS.
    pub alt: bool,
    /// Control.
    pub ctrl: bool,
    /// Meta, or Command on macOS.
    pub meta: bool,
}

impl Modifiers {
    /// No modifier held.
    pub const NONE: Self = Self {
        shift: false,
        alt: false,
        ctrl: false,
        meta: false,
    };

    /// Shift alone.
    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };
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
        super::keys::terminal_key_sequence(self, cursor_keys_application).map(String::into_bytes)
    }
}
