//! The controller contract: which W3C standard-mapping input produces which
//! intent, what each physical control does in words, and which intents repeat
//! while held. Data only — no DOM, no Gamepad objects — so the mapper
//! (`pad_mapper`), the transient legend and the controller map read ONE table.
//! Called by `pad_mapper`, `pad_router`, and the legend/map components.
//! Ported from `apps/web/src/lib/padBindings.ts`.

/// One controller intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PadAction {
    /// Focus travel up, or scroll a focused terminal until it clamps.
    MoveUp,
    /// Focus travel down, or scroll a focused terminal until it clamps.
    MoveDown,
    /// Focus travel left.
    MoveLeft,
    /// Focus travel right.
    MoveRight,
    /// Scroll the target pane's scrollback up; never moves focus.
    ScrollUp,
    /// Scroll the target pane's scrollback down; never moves focus.
    ScrollDown,
    /// A: select, press, or send the dictation.
    Activate,
    /// B: back, close, or discard the dictation.
    Back,
    /// X: toggle the command palette.
    Palette,
    /// Y: the focused item's context menu.
    ContextMenu,
    /// LB: previous tab in the target pane.
    TabPrev,
    /// RB: next tab in the target pane.
    TabNext,
    /// LT: previous pane in the layout.
    PanePrev,
    /// RT: next pane in the layout.
    PaneNext,
    /// Back/Select: toggle the terminal key pad.
    Keypad,
    /// Start: toggle the controller map.
    ControllerMap,
    /// L3: start or stop dictation.
    MicToggle,
    /// R3: the next folder's newest session.
    FolderNext,
}

impl PadAction {
    /// The v2 wire name, used in log lines and the controller map's keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MoveUp => "move-up",
            Self::MoveDown => "move-down",
            Self::MoveLeft => "move-left",
            Self::MoveRight => "move-right",
            Self::ScrollUp => "scroll-up",
            Self::ScrollDown => "scroll-down",
            Self::Activate => "activate",
            Self::Back => "back",
            Self::Palette => "palette",
            Self::ContextMenu => "context-menu",
            Self::TabPrev => "tab-prev",
            Self::TabNext => "tab-next",
            Self::PanePrev => "pane-prev",
            Self::PaneNext => "pane-next",
            Self::Keypad => "keypad",
            Self::ControllerMap => "controller-map",
            Self::MicToggle => "mic-toggle",
            Self::FolderNext => "folder-next",
        }
    }

    /// Whether the intent auto-repeats while held. Discrete commands never
    /// repeat — a held mic or folder button that fired every frame would be
    /// unusable.
    pub fn is_repeating(self) -> bool {
        matches!(
            self,
            Self::MoveUp
                | Self::MoveDown
                | Self::MoveLeft
                | Self::MoveRight
                | Self::ScrollUp
                | Self::ScrollDown
        )
    }
}

/// The intent bound to W3C standard-mapping button `index`; unlisted indices
/// (16, the optional guide button, and beyond) are unbound.
pub fn button_action(index: usize) -> Option<PadAction> {
    Some(match index {
        0 => PadAction::Activate,
        1 => PadAction::Back,
        2 => PadAction::Palette,
        3 => PadAction::ContextMenu,
        4 => PadAction::TabPrev,
        5 => PadAction::TabNext,
        6 => PadAction::PanePrev,
        7 => PadAction::PaneNext,
        8 => PadAction::Keypad,
        9 => PadAction::ControllerMap,
        10 => PadAction::MicToggle,
        11 => PadAction::FolderNext,
        12 => PadAction::MoveUp,
        13 => PadAction::MoveDown,
        14 => PadAction::MoveLeft,
        15 => PadAction::MoveRight,
        _ => return None,
    })
}

/// One physical control. Xbox-style caps for every vendor: `Gamepad.id` strings
/// are unreliable for vendor detection, and standard mapping fixes the indices
/// regardless. A legend row names controls by this type, so a legend cap the
/// controller map has no control for cannot be written at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadControl {
    A,
    B,
    X,
    Y,
    Lb,
    Rb,
    Lt,
    Rt,
    Back,
    Start,
    L3,
    R3,
    DPad,
    LStick,
    RStick,
}

impl PadControl {
    /// The printed cap, which is also the controller map's geometry key.
    pub fn cap(self) -> &'static str {
        self.guide().cap
    }

    /// This control's guide row. Constant indices into a fixed-size array are
    /// bounds-checked at compile time; the tests pin that each index lands on
    /// its own control's row.
    pub fn guide(self) -> &'static PadControlGuide {
        match self {
            Self::A => &PAD_CONTROL_GUIDE[0],
            Self::B => &PAD_CONTROL_GUIDE[1],
            Self::X => &PAD_CONTROL_GUIDE[2],
            Self::Y => &PAD_CONTROL_GUIDE[3],
            Self::Lb => &PAD_CONTROL_GUIDE[4],
            Self::Rb => &PAD_CONTROL_GUIDE[5],
            Self::Lt => &PAD_CONTROL_GUIDE[6],
            Self::Rt => &PAD_CONTROL_GUIDE[7],
            Self::Back => &PAD_CONTROL_GUIDE[8],
            Self::Start => &PAD_CONTROL_GUIDE[9],
            Self::L3 => &PAD_CONTROL_GUIDE[10],
            Self::R3 => &PAD_CONTROL_GUIDE[11],
            Self::DPad => &PAD_CONTROL_GUIDE[12],
            Self::LStick => &PAD_CONTROL_GUIDE[13],
            Self::RStick => &PAD_CONTROL_GUIDE[14],
        }
    }
}

/// One physical control, in words. `action` is `None` for the analogue
/// clusters, which produce a direction family rather than one intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadControlGuide {
    /// The control this row describes.
    pub control: PadControl,
    /// The printed cap.
    pub cap: &'static str,
    /// The single intent, if the control has one.
    pub action: Option<PadAction>,
    /// Legend verb — the legend vocabulary.
    pub label: &'static str,
    /// One clause spelling out the context-dependent behaviour.
    pub detail: &'static str,
}

const fn guide(
    control: PadControl,
    cap: &'static str,
    action: Option<PadAction>,
    label: &'static str,
    detail: &'static str,
) -> PadControlGuide {
    PadControlGuide {
        control,
        cap,
        action,
        label,
        detail,
    }
}

/// The ONLY place a control's wording lives: the legend derives its caps and
/// default verbs from these rows, so a legend that contradicts the controller
/// map cannot ship.
pub const PAD_CONTROL_GUIDE: [PadControlGuide; 15] = [
    guide(
        PadControl::A,
        "A",
        Some(PadAction::Activate),
        "Select",
        "Select the focused item; sends the dictation while recording; on a focused terminal it opens the key pad and lands on the first key",
    ),
    guide(
        PadControl::B,
        "B",
        Some(PadAction::Back),
        "Back",
        "Go back, close, or leave the terminal; discards the dictation while recording; closes the key pad and hands focus back to the terminal",
    ),
    guide(
        PadControl::X,
        "X",
        Some(PadAction::Palette),
        "Palette",
        "Toggle the command palette",
    ),
    guide(
        PadControl::Y,
        "Y",
        Some(PadAction::ContextMenu),
        "Menu",
        "Open the context menu for the focused item",
    ),
    guide(
        PadControl::Lb,
        "LB",
        Some(PadAction::TabPrev),
        "Tab",
        "Previous tab in the focused pane, cycling",
    ),
    guide(
        PadControl::Rb,
        "RB",
        Some(PadAction::TabNext),
        "Tab",
        "Next tab in the focused pane, cycling",
    ),
    guide(
        PadControl::Lt,
        "LT",
        Some(PadAction::PanePrev),
        "Pane",
        "Previous pane in the layout, cycling",
    ),
    guide(
        PadControl::Rt,
        "RT",
        Some(PadAction::PaneNext),
        "Pane",
        "Next pane in the layout, cycling",
    ),
    guide(
        PadControl::Back,
        "Back",
        Some(PadAction::Keypad),
        "Keys",
        "Toggle the terminal key pad; opening it focuses the first key so the pad can walk the keys",
    ),
    guide(
        PadControl::Start,
        "Start",
        Some(PadAction::ControllerMap),
        "Guide",
        "Toggle this controller guide",
    ),
    guide(
        PadControl::L3,
        "L3",
        Some(PadAction::MicToggle),
        "Mic",
        "Click the left stick to start dictation, or to stop and send it; the browser may need one tap of the on-screen mic first to grant the microphone",
    ),
    guide(
        PadControl::R3,
        "R3",
        Some(PadAction::FolderNext),
        "Folder",
        "Click the right stick to switch to the next folder, cycling by recent activity",
    ),
    guide(
        PadControl::DPad,
        "D-pad",
        None,
        "Move",
        "Move focus, or scroll a focused terminal until it reaches the edge",
    ),
    guide(
        PadControl::LStick,
        "L-stick",
        None,
        "Move",
        "Move focus like the D-pad, repeating while held",
    ),
    guide(
        PadControl::RStick,
        "R-stick",
        None,
        "Scroll",
        "Scroll the focused pane's terminal scrollback",
    ),
];
