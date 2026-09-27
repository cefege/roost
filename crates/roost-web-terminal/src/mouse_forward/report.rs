//! One browser pointer gesture, and the bytes a terminal application asked to
//! receive for it. `mouse_forward::forwarding` decides whether a gesture is
//! forwarded at all; this module owns the two wire formats and the value type
//! both speak.
//!
//! Nothing here reads the DOM and nothing touches a pane, so the whole matrix —
//! tracking mode, gesture kind, held-button state, modifier bypass, and both
//! encodings — is testable natively. Ported from v2's
//! `apps/web/src/renderer/terminalMouse.ts`.
//!
//! Both modes the foreground application can ask for come off the worker's cell
//! frame, never from alt-screen occupancy: vim, less and man occupy the alt
//! screen without ever requesting mouse reporting, and forwarding to them
//! swallowed the click with no native fallback.

use roost_protocol::cell::MouseTracking;

/// Cb bits. Shift and Alt are deliberately absent: they are Roost's per-gesture
/// bypass to native selection, so they never reach the application at all.
const CB_META: u8 = 8;
const CB_CTRL: u8 = 16;
const CB_MOTION: u8 = 32;
const CB_WHEEL_UP: u8 = 64;
const CB_WHEEL_DOWN: u8 = 65;
/// X10 has no per-button release; every release reports "all buttons up".
const CB_X10_RELEASE: u8 = 3;

/// Every biased X10 byte carries the cell 32 higher than it is named, and the
/// byte itself runs to 255 — so the largest nameable cell is xterm's
/// `MOUSE_LIMIT`.
const X10_BIAS: u8 = 32;
const X10_MAX_CELL: u32 = u32::from(u8::MAX) - u32::from(X10_BIAS);

const ESC: u8 = 0x1b;
const LEFT_BRACKET: u8 = 0x5b;
const CAPITAL_M: u8 = 0x4d;

/// The widest SGR report this crate can emit: `ESC [ < Cb ; Cx ; Cy M` with a
/// two-digit Cb and `u32` coordinates. X10 is six bytes, so one buffer covers
/// both formats and neither allocates.
pub const MAX_MOUSE_REPORT_BYTES: usize = 3 + 2 + 1 + 10 + 1 + 10 + 1;

/// The two mode bits the frame carries: what tracking the application asked for
/// and which encoding it wants for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReportModes {
    /// DECSET 1000/1002, read off the core.
    pub tracking: MouseTracking,
    /// Whether the frame asked for SGR-1006 rather than legacy X10.
    pub sgr: bool,
}

/// Which wire format a report is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseReportEncoding {
    /// DECSET 1006: `ESC [ < Cb ; Cx ; Cy M`, decimal, so coordinates are
    /// unbounded and a release ends in `m`.
    Sgr1006,
    /// Legacy X10: `ESC [ M` then three BYTES, each cell plus 32.
    LegacyX10,
}

/// A DOM `MouseEvent.button`, narrowed to the three the protocol reports.
///
/// The narrowing is the whole reason a back/forward press is left alone: those
/// buttons have no Cb in either format, so they are not representable here and
/// `mouse_button_from_dom` is the only place that has to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// DOM 0.
    Left,
    /// DOM 1. Encoded, but the forwarding layer reserves it for the deck's
    /// bring-to-front gesture and never sends a press for it.
    Middle,
    /// DOM 2.
    Right,
}

impl MouseButton {
    /// The button's Cb, before modifier and motion bits are folded in.
    const fn cb(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

/// Narrow a DOM `MouseEvent.button` to a reportable button.
///
/// DOM 3 and 4 are the back/forward buttons, and a negative value is not a
/// button at all: neither has a Cb in either wire format, so both are refused
/// here rather than carried into the encoder as a number it cannot use.
pub fn mouse_button_from_dom(button: i16) -> Option<MouseButton> {
    match button {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    }
}

/// The modifier keys one gesture carried. `shift` and `alt` are Roost's
/// per-gesture bypass to native selection and never become a Cb bit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    /// Selection bypass: leave this one gesture native.
    pub shift: bool,
    /// The other selection bypass, for layouts where Alt is the natural one.
    pub alt: bool,
    /// Cb bit 16.
    pub ctrl: bool,
    /// Cb bit 8.
    pub meta: bool,
}

/// Which way a wheel notch turned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelDirection {
    /// Toward history.
    Up,
    /// Toward the live tail.
    Down,
}

/// What a gesture does, and the button identity that goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseGestureKind {
    /// A button going down.
    Press {
        /// The button that went down.
        button: MouseButton,
    },
    /// The pointer moving. `held` says whether a button was still down, which is
    /// the whole difference between mode 1000 and mode 1002 for a motion.
    Motion {
        /// The button the drag belongs to.
        button: MouseButton,
        /// Whether that button is still down.
        held: bool,
    },
    /// A button coming up.
    Release {
        /// The button that came up.
        button: MouseButton,
    },
    /// One wheel notch, which carries no button.
    Wheel(WheelDirection),
}

/// One browser pointer or touch gesture, addressed at a grid cell.
///
/// `col` and `row` are 1-based because that is how the terminal numbers them;
/// `cell_from_point` is what derives them from a pointer position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReport {
    /// What the gesture does.
    pub kind: MouseGestureKind,
    /// The 1-based column under the pointer.
    pub col: u32,
    /// The 1-based row under the pointer.
    pub row: u32,
    /// The modifiers the event carried.
    pub modifiers: MouseModifiers,
}

impl MouseReport {
    /// A gesture at a cell with no modifiers held.
    pub const fn at(kind: MouseGestureKind, col: u32, row: u32) -> Self {
        Self {
            kind,
            col,
            row,
            modifiers: MouseModifiers {
                shift: false,
                alt: false,
                ctrl: false,
                meta: false,
            },
        }
    }
}

/// The exact bytes to write to the PTY, in a fixed buffer.
///
/// A `String` would be wrong here: X10 cells past 191 are bytes above 0x7f, and
/// a UTF-8 encoder would silently double every one of them on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedMouseReport {
    bytes: [u8; MAX_MOUSE_REPORT_BYTES],
    len: usize,
}

impl EncodedMouseReport {
    /// The encoded report.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// How many bytes the encoding produced.
    pub fn len(&self) -> usize {
        self.len
    }
}

/// Encode a gesture `mouse_forward::forwarding::should_forward` accepted.
///
/// Total by construction, because the rule has already refused everything this
/// cannot represent: the button is narrowed, the mode reported the gesture, and
/// `col`/`row` are clamped here rather than at every call site.
pub fn encode_mouse_report(
    encoding: MouseReportEncoding,
    gesture: &MouseReport,
) -> EncodedMouseReport {
    let mut report = EncodedMouseReport {
        bytes: [0; MAX_MOUSE_REPORT_BYTES],
        len: 0,
    };
    let col = gesture.col.max(1);
    let row = gesture.row.max(1);
    match encoding {
        MouseReportEncoding::Sgr1006 => {
            let (cb, release) = sgr_control_byte(gesture, true);
            push(&mut report, ESC);
            push(&mut report, LEFT_BRACKET);
            push(&mut report, b'<');
            push_decimal(&mut report, u32::from(cb));
            push(&mut report, b';');
            push_decimal(&mut report, col);
            push(&mut report, b';');
            push_decimal(&mut report, row);
            push(&mut report, if release { b'm' } else { CAPITAL_M });
        }
        MouseReportEncoding::LegacyX10 => {
            let (cb, _) = sgr_control_byte(gesture, false);
            push(&mut report, ESC);
            push(&mut report, LEFT_BRACKET);
            push(&mut report, CAPITAL_M);
            // Cb needs no bound: the widest value here is a ctrl+meta wheel
            // notch (89). Saturating anyway, so no future bit can overflow.
            push(&mut report, cb.saturating_add(X10_BIAS));
            push(&mut report, biased_cell(col));
            push(&mut report, biased_cell(row));
        }
    }
    report
}

/// The cell as the biased X10 byte carries it, saturating at `MOUSE_LIMIT`.
///
/// Clamping the biased BYTE at 223 would collapse every column past 191 onto
/// 191, which is inside the width of an ordinary pane; the byte itself runs to
/// 255 so it can name cell 223 as 255.
fn biased_cell(cell: u32) -> u8 {
    (cell.min(X10_MAX_CELL) + u32::from(X10_BIAS)) as u8
}

/// Cb for one gesture, plus whether it is a release (SGR's final byte).
///
/// Shared by both encodings so the two can never disagree about which gesture
/// is which. `sgr` selects the release identity: X10 has no per-button release
/// and reports "all buttons up" instead.
fn sgr_control_byte(gesture: &MouseReport, sgr: bool) -> (u8, bool) {
    let mut cb = match gesture.kind {
        MouseGestureKind::Wheel(direction) => match direction {
            WheelDirection::Up => CB_WHEEL_UP,
            WheelDirection::Down => CB_WHEEL_DOWN,
        },
        MouseGestureKind::Press { button } | MouseGestureKind::Motion { button, .. } => {
            button.cb()
        }
        MouseGestureKind::Release { button } if sgr => button.cb(),
        MouseGestureKind::Release { .. } => CB_X10_RELEASE,
    };
    if matches!(gesture.kind, MouseGestureKind::Motion { .. }) {
        cb |= CB_MOTION;
    }
    if gesture.modifiers.meta {
        cb |= CB_META;
    }
    if gesture.modifiers.ctrl {
        cb |= CB_CTRL;
    }
    let release = matches!(gesture.kind, MouseGestureKind::Release { .. });
    (cb, release)
}

/// Append one byte. The bound check is unreachable — the widest SGR report is
/// exactly `MAX_MOUSE_REPORT_BYTES` and X10 writes six — but this crate forbids
/// a panicking path, so a byte that would not fit is dropped rather than
/// indexed out of bounds.
fn push(report: &mut EncodedMouseReport, byte: u8) {
    if report.len < report.bytes.len() {
        report.bytes[report.len] = byte;
        report.len += 1;
    }
}

/// Write `value` as decimal, digits first into a stack buffer, so the whole
/// encoding is one pass with no intermediate `String` to build and drop.
fn push_decimal(report: &mut EncodedMouseReport, value: u32) {
    let mut digits = [0u8; 10];
    let mut start = digits.len();
    let mut remaining = value;
    loop {
        start -= 1;
        digits[start] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for digit in &digits[start..] {
        push(report, *digit);
    }
}
