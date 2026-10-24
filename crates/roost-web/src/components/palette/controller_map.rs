//! The Start-button overlay: a controller silhouette whose caps sit where they
//! sit on the hardware, each naming its action and each lit while its physical
//! button is held, so a reader learns their own pad. Ports
//! `apps/web/src/components/palette/ControllerMap.tsx`; the wording is read from
//! `input_nav::pad_bindings::PAD_CONTROL_GUIDE` and only the GEOMETRY is this
//! file's, so the transient legend in `pad_hints` cannot disagree with the map.
//!
//! There is no text field anywhere in it. An autofocused filter is what makes
//! the help overlay a dead end for someone holding a pad instead of a keyboard.

use std::collections::BTreeSet;

use dioxus::prelude::*;

use libero::components::Dialog;
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;

use crate::input_nav::pad_bindings::{PAD_CONTROL_GUIDE, PadAction, PadControlGuide};
use crate::input_nav::{NavModality, PadHeld};

#[path = "controller_map_view.rs"]
mod view;
use view::ControllerMapBody;

/// One seat of the diagram: a cap, the cluster it sits in, and the
/// standard-mapping indices that light it.
///
/// The analogue sticks travel on axes, so only their click carries an index.
struct PadMapSlot {
    cap: &'static str,
    cluster: &'static str,
    buttons: &'static [usize],
}

const SHOULDER_LEFT: &str = "shoulder-left";
const SHOULDER_RIGHT: &str = "shoulder-right";
const STICK_LEFT: &str = "stick-left";
const DPAD: &str = "dpad";
const CENTRE: &str = "centre";
const FACE: &str = "face";
const STICK_RIGHT: &str = "stick-right";

/// Source order is DOM order within a cluster. The face diamond is the one
/// exception: its four seats form a diamond in `controller_map_view`.
const PAD_MAP_SLOTS: [PadMapSlot; 15] = [
    PadMapSlot {
        cap: "LT",
        cluster: SHOULDER_LEFT,
        buttons: &[6],
    },
    PadMapSlot {
        cap: "LB",
        cluster: SHOULDER_LEFT,
        buttons: &[4],
    },
    PadMapSlot {
        cap: "RT",
        cluster: SHOULDER_RIGHT,
        buttons: &[7],
    },
    PadMapSlot {
        cap: "RB",
        cluster: SHOULDER_RIGHT,
        buttons: &[5],
    },
    PadMapSlot {
        cap: "L-stick",
        cluster: STICK_LEFT,
        buttons: &[],
    },
    PadMapSlot {
        cap: "L3",
        cluster: STICK_LEFT,
        buttons: &[10],
    },
    PadMapSlot {
        cap: "D-pad",
        cluster: DPAD,
        buttons: &[12, 13, 14, 15],
    },
    PadMapSlot {
        cap: "Back",
        cluster: CENTRE,
        buttons: &[8],
    },
    PadMapSlot {
        cap: "Start",
        cluster: CENTRE,
        buttons: &[9],
    },
    PadMapSlot {
        cap: "Y",
        cluster: FACE,
        buttons: &[3],
    },
    PadMapSlot {
        cap: "X",
        cluster: FACE,
        buttons: &[2],
    },
    PadMapSlot {
        cap: "B",
        cluster: FACE,
        buttons: &[1],
    },
    PadMapSlot {
        cap: "A",
        cluster: FACE,
        buttons: &[0],
    },
    PadMapSlot {
        cap: "R-stick",
        cluster: STICK_RIGHT,
        buttons: &[],
    },
    PadMapSlot {
        cap: "R3",
        cluster: STICK_RIGHT,
        buttons: &[11],
    },
];

/// The caps whose sticks travel on axes, and so are lit by intent rather than
/// by a button index.
const DPAD_CAP: &str = "D-pad";
const STICK_LEFT_CAP: &str = "L-stick";
const STICK_RIGHT_CAP: &str = "R-stick";

const PAD_MAP_CLUSTERS: [&str; 7] = [
    SHOULDER_LEFT,
    SHOULDER_RIGHT,
    STICK_LEFT,
    DPAD,
    CENTRE,
    FACE,
    STICK_RIGHT,
];

/// A button no cap claims is still reported: seeing "Button 16" light up is how
/// a reader learns their pad has one this build binds nothing to.
const UNBOUND_LABEL: &str = "Unbound";
const UNBOUND_DETAIL: &str = "This pad reports this button; nothing is bound to it";
const IDLE_CAPTION: &str = "Hold a button to read exactly what it does.";

/// No index can light a stick, so an intent identifies it: a scroll can only
/// have come from the right stick, and a move with no D-pad index held from the
/// left.
const STICK_MOVE_ACTIONS: [PadAction; 4] = [
    PadAction::MoveUp,
    PadAction::MoveDown,
    PadAction::MoveLeft,
    PadAction::MoveRight,
];
const STICK_SCROLL_ACTIONS: [PadAction; 2] = [PadAction::ScrollUp, PadAction::ScrollDown];

/// The Start-button overlay.
#[component]
pub fn ControllerMap() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    let modality: Option<Signal<NavModality>> = try_use_context();
    let held: Option<Signal<PadHeld>> = try_use_context();
    let mut product_open = overlays.controller_map;
    let dialog_sx = use_hook(|| sx().max_width("65rem"));
    let modal = use_modal(move |_scope: ModalScope<()>| {
        let held = held.and_then(|held| held.try_read().ok().map(|value| value.cloned()));
        rsx! {
            Dialog { title: "Controller map", sx: dialog_sx.clone(),
                ControllerMapBody { held: held.unwrap_or_default() }
            }
        }
    });
    let mut previously_open = use_signal(|| false);
    use_effect(move || {
        let requested_open = *product_open.read()
            && modality.is_some_and(|modality| modality.read().pad_input_seen());
        let modal_open = modal.is_open();
        if requested_open {
            if modal_open {
                previously_open.set(true);
            } else if previously_open() {
                product_open.set(false);
                previously_open.set(false);
            } else {
                modal.open();
                previously_open.set(true);
            }
        } else {
            if modal_open {
                modal.close();
            }
            previously_open.set(false);
        }
    });
    rsx! {}
}

/// The caps the current poll is holding.
fn held_caps(held: &PadHeld) -> BTreeSet<&'static str> {
    let mut caps: BTreeSet<&'static str> = PAD_MAP_SLOTS
        .iter()
        .filter(|slot| {
            slot.buttons
                .iter()
                .any(|index| held.buttons.contains(index))
        })
        .map(|slot| slot.cap)
        .collect();
    if STICK_SCROLL_ACTIONS
        .iter()
        .any(|action| held.actions.contains(action))
    {
        caps.insert(STICK_RIGHT_CAP);
    }
    if !caps.contains(DPAD_CAP)
        && STICK_MOVE_ACTIONS
            .iter()
            .any(|action| held.actions.contains(action))
    {
        caps.insert(STICK_LEFT_CAP);
    }
    caps
}

/// What the held cap does, in the guide's own words.
fn caption_for(caps: &BTreeSet<&'static str>) -> String {
    let Some(slot) = PAD_MAP_SLOTS.iter().find(|slot| caps.contains(slot.cap)) else {
        return IDLE_CAPTION.to_owned();
    };
    match guide_for(slot.cap) {
        Some(guide) => format!("{} · {} — {}", guide.cap, guide.label, guide.detail),
        None => IDLE_CAPTION.to_owned(),
    }
}

/// The guide row for a cap; the ONLY place a control's wording lives.
fn guide_for(cap: &str) -> Option<PadControlGuide> {
    PAD_CONTROL_GUIDE
        .iter()
        .copied()
        .find(|guide| guide.cap == cap)
}
