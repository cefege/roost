//! One poll of a standard-mapping pad → the intents to run now, with press /
//! repeat semantics, plus the held state the controller map highlights.
//!
//! Pure: a [`PadSnapshot`] is plain data, never a `Gamepad` object, so the
//! rules a controller depends on are native-tested. Called by the wasm poll
//! loop in `gamepad_source`. Depends on `pad_bindings`.
//! Ported from the pure half of `apps/web/src/browser/gamepadSource.ts`.

use std::collections::BTreeMap;

use crate::input_nav::pad_bindings::{PadAction, button_action};

/// A stick counts as pushed at this deflection, not before.
pub const PAD_AXIS_DEADZONE: f64 = 0.35;
/// A held repeating intent fires again this long after the press.
pub const PAD_REPEAT_DELAY_MS: f64 = 400.0;
/// …and then every this often while still held.
pub const PAD_REPEAT_INTERVAL_MS: f64 = 110.0;

/// Bound axis → (index, intent past −deadzone, intent past +deadzone).
/// Standard mapping: 0/1 = left stick X/Y, 2/3 = right stick X/Y; axis 2 is
/// unbound. A list, not a map: its order also fixes the held-signature bits.
const AXIS_ACTIONS: [(usize, PadAction, PadAction); 3] = [
    (0, PadAction::MoveLeft, PadAction::MoveRight),
    (1, PadAction::MoveUp, PadAction::MoveDown),
    (3, PadAction::ScrollUp, PadAction::ScrollDown),
];

/// Buttons past this many cannot trigger a republish alone. Standard mapping
/// defines 17 (0–16, where 16 is the optional guide button).
const SIGNATURE_BUTTON_BITS: usize = 24;

/// One poll's worth of pad state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PadSnapshot {
    /// `pressed` per standard-mapping button index.
    pub buttons: Vec<bool>,
    /// Axis values in `[-1, 1]` per standard-mapping axis index.
    pub axes: Vec<f64>,
}

/// Every intent held in this snapshot, before any press/repeat gating, buttons
/// by index then axes. The controller map highlights what is held, not what
/// fired.
pub fn held_pad_actions(snapshot: &PadSnapshot) -> Vec<PadAction> {
    let mut held: Vec<PadAction> = snapshot
        .buttons
        .iter()
        .enumerate()
        .filter(|(_, pressed)| **pressed)
        .filter_map(|(index, _)| button_action(index))
        .collect();
    for (index, negative, positive) in AXIS_ACTIONS {
        let value = snapshot.axes.get(index).copied().unwrap_or(0.0);
        if value <= -PAD_AXIS_DEADZONE {
            held.push(negative);
        } else if value >= PAD_AXIS_DEADZONE {
            held.push(positive);
        }
    }
    held
}

/// Per-intent hold state carried between polls: when each held intent may
/// fire next, or `None` for a discrete intent that never fires again while held.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PadHoldState {
    next_fire_ms: BTreeMap<PadAction, Option<f64>>,
}

impl PadHoldState {
    /// Nothing held.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many intents are being held.
    pub fn len(&self) -> usize {
        self.next_fire_ms.len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.next_fire_ms.is_empty()
    }

    /// Forget every hold, when the poll loop stops.
    pub fn clear(&mut self) {
        self.next_fire_ms.clear();
    }

    /// The intents to run for this poll: a fresh press, or a hold past its
    /// repeat gate. A released intent is forgotten, so its next press fires at
    /// once.
    pub fn actions_to_fire(&mut self, snapshot: &PadSnapshot, now_ms: f64) -> Vec<PadAction> {
        let held = held_pad_actions(snapshot);
        let mut fire = Vec::new();
        for action in held.iter().copied() {
            match self.next_fire_ms.get(&action).copied() {
                None => {
                    fire.push(action);
                    // A non-repeating intent fires once and never again while
                    // held; the same press from two inputs (D-pad + stick)
                    // dedupes here too.
                    let next = action.is_repeating().then_some(now_ms + PAD_REPEAT_DELAY_MS);
                    self.next_fire_ms.insert(action, next);
                }
                Some(Some(next_fire)) if now_ms >= next_fire => {
                    fire.push(action);
                    self.next_fire_ms
                        .insert(action, Some(now_ms + PAD_REPEAT_INTERVAL_MS));
                }
                Some(_) => {}
            }
        }
        self.next_fire_ms.retain(|action, _| held.contains(action));
        fire
    }
}

/// What the controller map highlights: raw button indices, unbound ones
/// included, and the intents they hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadHeld {
    /// Held standard-mapping indices, ascending. Includes unbound ones: seeing
    /// 10 and 11 light up is how a user learns whether their pad reports the
    /// stick clicks this build binds the mic and folders to.
    pub buttons: Vec<usize>,
    /// Held intents. The map highlights by intent, so a D-pad press and a stick
    /// push light the same row.
    pub actions: Vec<PadAction>,
}

/// Publishes held state only when its membership changed.
///
/// The signature compare is the point: a fresh value every frame would
/// re-render the whole controller map at 60 fps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeldPublisher {
    signature: u64,
}

impl HeldPublisher {
    /// Nothing published.
    pub fn new() -> Self {
        Self::default()
    }

    /// The held state to publish for `snapshot` (`None` when the loop stopped),
    /// or `None` when it is the membership already published.
    pub fn publish(&mut self, snapshot: Option<&PadSnapshot>) -> Option<PadHeld> {
        let signature = snapshot.map_or(0, held_signature);
        if signature == self.signature {
            return None;
        }
        self.signature = signature;
        Some(snapshot.map_or_else(PadHeld::default, |snapshot| PadHeld {
            buttons: snapshot
                .buttons
                .iter()
                .enumerate()
                .filter(|(_, pressed)| **pressed)
                .map(|(index, _)| index)
                .collect(),
            actions: held_pad_actions(snapshot),
        }))
    }
}

/// One integer per distinct held state: a bit per button index, then two bits
/// per bound axis for its deadzone crossings.
fn held_signature(snapshot: &PadSnapshot) -> u64 {
    let mut bits = 0_u64;
    for (index, pressed) in snapshot.buttons.iter().take(SIGNATURE_BUTTON_BITS).enumerate() {
        if *pressed {
            bits |= 1 << index;
        }
    }
    for (slot, (index, _, _)) in AXIS_ACTIONS.iter().enumerate() {
        let value = snapshot.axes.get(*index).copied().unwrap_or(0.0);
        let base = SIGNATURE_BUTTON_BITS + slot * 2;
        if value <= -PAD_AXIS_DEADZONE {
            bits |= 1 << base;
        } else if value >= PAD_AXIS_DEADZONE {
            bits |= 1 << (base + 1);
        }
    }
    bits
}
