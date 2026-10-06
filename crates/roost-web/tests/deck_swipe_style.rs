//! The compact deck's swipe styles against the Dioxus style merge: every
//! element a swipe moves declares one property set, placement plus swipe
//! layer, in every swipe state, so the frame a swipe last painted cannot
//! outlive it. The FAILURE-INDEX entry "A Dioxus `style` string keeps every
//! property the previous render set".

use roost_client_core::deck::{DeckSize, TerminalSessionSlot};
use roost_client_core::store::layout::PaneRect;
use roost_web::components::deck::deck_swipe::{
    SettleTarget, Swipe, SwipeDirection, SwipeMode, SwipePhase,
};
use roost_web::components::deck::deck_swipe_style::{mobile_bar_style, swipe_style_for};
use roost_web::components::deck::terminal_deck_geometry::terminal_session_style;

const WIDTH: f64 = 390.0;
const DECK: DeckSize = DeckSize { w: WIDTH, h: 551.0 };

fn phone_slot() -> TerminalSessionSlot {
    TerminalSessionSlot {
        rect: PaneRect {
            x: 0.0,
            y: 0.0,
            w: WIDTH,
            h: 551.0,
        },
        pane_id: "only".to_owned(),
        focused: true,
        spotlit: false,
    }
}

fn slide(phase: SwipePhase, settle_target: Option<SettleTarget>) -> Swipe {
    Swipe {
        phase,
        current_id: "cur".to_owned(),
        neighbor_id: Some("nxt".to_owned()),
        dir: SwipeDirection::Next,
        offset: -150.0,
        mode: SwipeMode::Slide,
        settle_target,
        settle_ms: settle_target.map(|_| 200),
    }
}

fn pull(phase: SwipePhase, settle_target: Option<SettleTarget>) -> Swipe {
    Swipe {
        neighbor_id: None,
        mode: SwipeMode::NewTerminal,
        ..slide(phase, settle_target)
    }
}

/// Every swipe state an element lives through, the no-swipe rest first.
fn every_swipe_state() -> Vec<Option<Swipe>> {
    let mut states = vec![None];
    for target in [None, Some(SettleTarget::Commit), Some(SettleTarget::Cancel)] {
        let phase = if target.is_some() {
            SwipePhase::Settle
        } else {
            SwipePhase::Track
        };
        states.push(Some(slide(phase, target)));
        states.push(Some(pull(phase, target)));
    }
    states.push(Some(Swipe {
        mode: SwipeMode::Workspace,
        neighbor_id: None,
        dir: SwipeDirection::Previous,
        offset: 120.0,
        ..slide(SwipePhase::Track, None)
    }));
    states
}

/// The property names a style declares, as the browser reads its `style` text.
fn declared_properties(style: &str) -> Vec<String> {
    let mut names: Vec<String> = style
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .map(|(name, _)| name.trim().to_owned())
        .collect();
    names.sort();
    names
}

/// A slot that was the current card or the neighbour of a swipe keeps no
/// transform, shadow or transition once the swipe is gone: a slid-away slot
/// kept `translateX(-width)` and a later tap revealed it off-screen.
#[test]
fn a_slot_declares_one_property_set_through_every_swipe_state() {
    let base = terminal_session_style(Some(&phone_slot()), None, DECK, 48.0, true);
    let rest = declared_properties(
        &base
            .clone()
            .merged(&swipe_style_for(None, "cur", WIDTH))
            .css(),
    );
    for state in every_swipe_state() {
        for session_id in ["cur", "nxt", "other"] {
            let style = base
                .clone()
                .merged(&swipe_style_for(state.as_ref(), session_id, WIDTH));
            assert_eq!(
                declared_properties(&style.css()),
                rest,
                "{session_id} under {state:?}"
            );
        }
    }
    let at_rest = base.merged(&swipe_style_for(None, "cur", WIDTH));
    assert_eq!(at_rest.get("transform"), Some("none"));
    assert_eq!(at_rest.get("transition"), Some("none"));
    assert_eq!(at_rest.get("box-shadow"), Some("none"));
}

/// The phone bar's wrapper is one element for the deck's life, whichever tab
/// it shows: a committed slide left it a width off-screen, so the bar was
/// gone, and a new-terminal pull left it rounded and clipped.
#[test]
fn the_phone_bar_declares_one_property_set_through_every_swipe_state() {
    let rest = declared_properties(&mobile_bar_style(None, "cur", WIDTH).css());
    for state in every_swipe_state() {
        for session_id in ["cur", "nxt", "other"] {
            let style = mobile_bar_style(state.as_ref(), session_id, WIDTH);
            assert_eq!(
                declared_properties(&style.css()),
                rest,
                "{session_id} under {state:?}"
            );
        }
    }
    let at_rest = mobile_bar_style(None, "cur", WIDTH);
    assert_eq!(at_rest.get("transform"), Some("none"));
    assert_eq!(at_rest.get("border-radius"), Some("0"));
    assert_eq!(at_rest.get("overflow"), Some("visible"));
    let peeled = mobile_bar_style(Some(&pull(SwipePhase::Track, None)), "cur", WIDTH);
    assert_eq!(
        peeled.get("overflow"),
        Some("hidden"),
        "the peel still clips"
    );
}
