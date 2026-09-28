//! The context-sensitive control hints: which physical controls the transient
//! legend names in each surface context (default, menu, overlay, terminal,
//! keypad, dictation), read from the one binding table in `pad_bindings`.
//! Called by `pad_router` and the legend component.
//! Ported from `apps/web/src/lib/padBindings.ts` (`padHints`).

use crate::input_nav::pad_bindings::PadControl;

/// Which legend the controller shows, by what focus is inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PadHintContext {
    /// Anywhere without a more specific surface.
    #[default]
    Default,
    /// A menu or dialog.
    Menu,
    /// The command palette or help overlay.
    Overlay,
    /// A focused terminal scroll box.
    Terminal,
    /// Inside the terminal key pad.
    Keypad,
    /// While a dictation is owned.
    Dictation,
}

impl PadHintContext {
    /// Every context, for a legend or a test that walks them all.
    pub const ALL: [Self; 6] = [
        Self::Default,
        Self::Menu,
        Self::Overlay,
        Self::Terminal,
        Self::Keypad,
        Self::Dictation,
    ];

    /// The v2 name, for log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Menu => "menu",
            Self::Overlay => "overlay",
            Self::Terminal => "terminal",
            Self::Keypad => "keypad",
            Self::Dictation => "dictation",
        }
    }
}

/// One legend row: caps joined the way a user reads them (`LB/RB`) and a verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadHint {
    /// The joined caps.
    pub cap: String,
    /// The verb.
    pub label: &'static str,
}

type HintSpec = (&'static [PadControl], Option<&'static str>);

use PadControl as C;

const DEFAULT_HINTS: &[HintSpec] = &[
    (&[C::DPad], None),
    (&[C::A], None),
    (&[C::B], None),
    (&[C::X], None),
    (&[C::Y], None),
    (&[C::Lb, C::Rb], None),
    (&[C::Lt, C::Rt], None),
    (&[C::L3], None),
    (&[C::R3], None),
    (&[C::Start], None),
];
const MENU_HINTS: &[HintSpec] = &[
    (&[C::DPad], None),
    (&[C::A], Some("Choose")),
    (&[C::B], Some("Close")),
];
const OVERLAY_HINTS: &[HintSpec] = &[
    (&[C::DPad], None),
    (&[C::A], Some("Open")),
    (&[C::B], Some("Close")),
];
const TERMINAL_HINTS: &[HintSpec] = &[
    (&[C::DPad], Some("Scroll")),
    (&[C::RStick], None),
    (&[C::A], Some("Keys")),
    (&[C::B], None),
    (&[C::Lb, C::Rb], None),
    (&[C::Lt, C::Rt], None),
    (&[C::L3], None),
    (&[C::R3], None),
    (&[C::Start], None),
];
const KEYPAD_HINTS: &[HintSpec] = &[
    (&[C::DPad], None),
    (&[C::A], Some("Press")),
    (&[C::B], Some("Close")),
];
const DICTATION_HINTS: &[HintSpec] = &[
    (&[C::A], Some("Send")),
    (&[C::B], Some("Discard")),
    (&[C::L3], Some("Stop")),
];

/// The legend for `context`. A row that does not override its verb reuses the
/// first control's guide label, so a context spells out only what it changes.
pub fn pad_hints(context: PadHintContext) -> Vec<PadHint> {
    let specs = match context {
        PadHintContext::Default => DEFAULT_HINTS,
        PadHintContext::Menu => MENU_HINTS,
        PadHintContext::Overlay => OVERLAY_HINTS,
        PadHintContext::Terminal => TERMINAL_HINTS,
        PadHintContext::Keypad => KEYPAD_HINTS,
        PadHintContext::Dictation => DICTATION_HINTS,
    };
    specs
        .iter()
        .map(|(controls, label)| PadHint {
            cap: controls
                .iter()
                .map(|control| control.cap())
                .collect::<Vec<_>>()
                .join("/"),
            label: label
                .unwrap_or_else(|| controls.first().map_or("", |first| first.guide().label)),
        })
        .collect()
}
