//! Directional-input modality: TV mode and game-controller mode, and the one
//! predicate "a non-pointer device is driving DOM focus".
//!
//! Owns both persisted choices (`?tv=` / `?pad=` win and persist, because a
//! console or TV has no reachable Settings before its first load), the TV
//! user-agent heuristic, and the "a pad actually sent input" latch. Pure: the
//! browser reads (query, UA) and the root-attribute writes live in
//! `modality_dom`. Called by `spatial_dom`, `gamepad_source`, `pad_router`, the
//! terminal components and Settings. Depends on `roost_client_core::KeyValueStore`.
//! Ported from `apps/web/src/lib/{tvMode,padMode,directionalInput}.ts`.

use roost_client_core::KeyValueStore;

/// The stored TV choice.
pub const TV_MODE_KEY: &str = "roost.tvMode";
/// The bootstrap query parameter for TV mode.
pub const TV_MODE_PARAM: &str = "tv";
/// The stored controller choice.
pub const PAD_MODE_KEY: &str = "roost.padMode";
/// The bootstrap query parameter for controller mode.
pub const PAD_MODE_PARAM: &str = "pad";

/// Below this width a `(pointer: none)` match is far more likely a phone with
/// a broken media-query implementation than a television.
pub const TV_MIN_POINTERLESS_WIDTH_PX: f64 = 1280.0;

/// A persisted three-way switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModeChoice {
    /// Decided by the device: the TV user agent, or the first real pad input.
    #[default]
    Auto,
    /// Forced on.
    On,
    /// Forced off.
    Off,
}

impl ModeChoice {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// A stored value; anything this build did not write is `None`.
    pub fn parse_stored(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// A query-parameter value: `1`/`on`/`true`, `0`/`off`/`false`, `auto`.
    pub fn parse_param(value: &str) -> Option<Self> {
        match value {
            "1" | "on" | "true" => Some(Self::On),
            "0" | "off" | "false" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Read one persisted choice. A recognised query value wins AND is persisted,
/// so the one-time URL typed on a TV's on-screen keyboard survives every later
/// load; otherwise the stored value; otherwise `Auto`.
pub fn load_mode_choice(param: Option<&str>, storage: &dyn KeyValueStore, key: &str) -> ModeChoice {
    if let Some(choice) = param.and_then(ModeChoice::parse_param) {
        storage.set(key, choice.as_str());
        return choice;
    }
    storage
        .get(key)
        .as_deref()
        .and_then(ModeChoice::parse_stored)
        .unwrap_or_default()
}

/// Vendor tokens observed in smart-TV / set-top user agents, matched
/// case-insensitively on word boundaries. A TV missing from this list that
/// also reports a pointer is a false negative the user fixes with `?tv=1`;
/// widening the heuristic instead would catch real desktops.
const TV_TOKENS: [&str; 19] = [
    "smart-tv",
    "smarttv",
    "googletv",
    "androidtv",
    "appletv",
    "crkey",
    "hbbtv",
    "netcast",
    "web0s",
    "webos",
    "tizen",
    "viera",
    "aquos",
    "bravia",
    "philipstv",
    "roku",
    "nettv",
    "dtv",
    "android tv",
];

/// Whether `user_agent` names a smart-TV / set-top browser.
///
/// The UA is the ONLY auto signal. A pointerless wide viewport is deliberately
/// not sufficient: automation browsers and pointer-less desktops report
/// `(pointer: none)` at any width, and that branch once switched real desktops
/// into a D-pad UI that suppresses PTY focus.
pub fn matches_tv_user_agent(user_agent: &str) -> bool {
    let lowered = user_agent.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    (0..bytes.len()).any(|start| {
        let starts_word = start == 0 || !is_word_byte(bytes[start - 1]);
        starts_word
            && TV_TOKENS
                .iter()
                .any(|token| token_at(&lowered, start, token))
    })
}

/// Whether the viewport looks like a pointerless television. Diagnostic only —
/// see [`matches_tv_user_agent`] for why it never switches the mode.
pub fn pointerless_tv_viewport(pointer_none: bool, inner_width_px: f64) -> bool {
    pointer_none && inner_width_px >= TV_MIN_POINTERLESS_WIDTH_PX
}

/// The directional-input modality of this tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NavModality {
    tv_choice: ModeChoice,
    pad_choice: ModeChoice,
    tv_user_agent: bool,
    pad_seen: bool,
}

impl NavModality {
    /// Both choices, and whether the UA is a TV's. The pad latch starts clear:
    /// a pad plugged in for games must not flip the mode on connection alone.
    pub fn new(tv_choice: ModeChoice, pad_choice: ModeChoice, tv_user_agent: bool) -> Self {
        Self {
            tv_choice,
            pad_choice,
            tv_user_agent,
            pad_seen: false,
        }
    }

    /// Load both choices from the query values and storage.
    pub fn load(
        tv_param: Option<&str>,
        pad_param: Option<&str>,
        storage: &dyn KeyValueStore,
        tv_user_agent: bool,
    ) -> Self {
        Self::new(
            load_mode_choice(tv_param, storage, TV_MODE_KEY),
            load_mode_choice(pad_param, storage, PAD_MODE_KEY),
            tv_user_agent,
        )
    }

    /// The TV choice, for the Settings picker.
    pub fn tv_choice(&self) -> ModeChoice {
        self.tv_choice
    }

    /// The controller choice, for the Settings picker and the poll gate.
    pub fn pad_choice(&self) -> ModeChoice {
        self.pad_choice
    }

    /// Whether the UA matched the TV list.
    pub fn tv_user_agent(&self) -> bool {
        self.tv_user_agent
    }

    /// Whether the ten-foot UI is on.
    pub fn tv_mode_active(&self) -> bool {
        match self.tv_choice {
            ModeChoice::On => true,
            ModeChoice::Off => false,
            ModeChoice::Auto => self.tv_user_agent,
        }
    }

    /// Whether controller mode is on.
    pub fn pad_mode_active(&self) -> bool {
        match self.pad_choice {
            ModeChoice::On => true,
            ModeChoice::Off => false,
            ModeChoice::Auto => self.pad_seen,
        }
    }

    /// Did a pad actually send input? [`Self::pad_mode_active`] conflates the
    /// latch with "forced on", so a surface that must appear ONLY for a real
    /// controller (the Start guide) reads this instead.
    pub fn pad_input_seen(&self) -> bool {
        self.pad_seen
    }

    /// THE predicate: TV remotes and controllers both suppress PTY auto-focus,
    /// make the terminal scroll box focusable, and hand the arrows to spatial
    /// navigation.
    pub fn directional_input_active(&self) -> bool {
        self.tv_mode_active() || self.pad_mode_active()
    }

    /// Latch `Auto` on after the first real press or out-of-deadzone axis.
    /// Returns whether the latch moved (the root attribute must be re-applied).
    pub fn note_pad_activity(&mut self) -> bool {
        if self.pad_seen {
            return false;
        }
        self.pad_seen = true;
        true
    }

    /// Persist and set the controller choice.
    pub fn set_pad_choice(&mut self, storage: &dyn KeyValueStore, choice: ModeChoice) {
        storage.set(PAD_MODE_KEY, choice.as_str());
        self.pad_choice = choice;
    }

    /// Persist and set the TV choice.
    pub fn set_tv_choice(&mut self, storage: &dyn KeyValueStore, choice: ModeChoice) {
        storage.set(TV_MODE_KEY, choice.as_str());
        self.tv_choice = choice;
    }

    /// The `data-tv` / `data-pad` values `tv.css` and `gamepad.css` key off.
    pub fn root_attributes(&self) -> [(&'static str, &'static str); 2] {
        let flag = |on: bool| if on { "true" } else { "false" };
        [
            ("data-tv", flag(self.tv_mode_active())),
            ("data-pad", flag(self.pad_mode_active())),
        ]
    }
}

/// JS `\w` without the unicode flag.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `token` occurs at `start` and ends on a word boundary. The one
/// token with a space stands for `android\s?tv`, so any single whitespace
/// character matches it.
fn token_at(haystack: &str, start: usize, token: &str) -> bool {
    let Some(rest) = haystack.get(start..) else {
        return false;
    };
    let matched_len = match token.split_once(' ') {
        None => rest.starts_with(token).then_some(token.len()),
        Some((head, tail)) => rest.strip_prefix(head).and_then(|after| {
            let gap = after.chars().next().filter(|gap| gap.is_whitespace())?;
            after[gap.len_utf8()..]
                .starts_with(tail)
                .then_some(head.len() + gap.len_utf8() + tail.len())
        }),
    };
    matched_len.is_some_and(|len| {
        haystack
            .as_bytes()
            .get(start + len)
            .is_none_or(|next| !is_word_byte(*next))
    })
}
