//! Where a terminal slot paints. The parked case is the FAILURE-INDEX entry "A
//! parked pane paints at a lying box size": a hidden pane must stay laid out
//! at the terminal area it will be revealed at, never a fixed box. Pins
//! `terminalSessionStyle` from `apps/web/src/components/deck/terminal-deck-geometry.ts`.

use roost_client_core::deck::{DeckSize, TerminalSessionSlot};
use roost_client_core::store::layout::PaneRect;
use roost_web::components::deck::terminal_deck_geometry::terminal_session_style;

const DECK: DeckSize = DeckSize {
    w: 1200.0,
    h: 800.0,
};

fn slot(focused: bool, spotlit: bool) -> TerminalSessionSlot {
    TerminalSessionSlot {
        rect: PaneRect {
            x: 603.0,
            y: 0.0,
            w: 597.0,
            h: 800.0,
        },
        pane_id: "right".to_owned(),
        focused,
        spotlit,
    }
}

#[test]
fn a_parked_terminal_stays_laid_out_at_its_reveal_size() {
    let park = DeckSize { w: 597.0, h: 765.0 };
    let parked = terminal_session_style(None, Some(park), DECK, 35.0);
    assert_eq!(parked.get("width"), Some("597px"));
    assert_eq!(parked.get("height"), Some("765px"));
    assert_eq!(parked.get("visibility"), Some("hidden"));
    assert_eq!(
        parked.get("left"),
        Some("-99999px"),
        "off-screen, not display:none"
    );
    assert_eq!(parked.get("display"), None);
    let revealed = terminal_session_style(Some(&slot(true, false)), None, DECK, 35.0);
    assert_eq!(
        revealed.get("height"),
        parked.get("height"),
        "the box does not change on reveal"
    );
    assert_eq!(revealed.get("width"), parked.get("width"));
}

#[test]
fn an_unparked_terminal_falls_back_to_the_deck_then_a_default_box() {
    let fallback = terminal_session_style(None, None, DECK, 35.0);
    assert_eq!(
        (fallback.get("width"), fallback.get("height")),
        (Some("1200px"), Some("765px"))
    );
    let unmeasured = terminal_session_style(None, None, DeckSize::default(), 35.0);
    assert_eq!(
        (unmeasured.get("width"), unmeasured.get("height")),
        (Some("800px"), Some("600px"))
    );
}

#[test]
fn a_slotted_terminal_sits_below_its_strip_and_the_focused_one_on_top() {
    let focused = terminal_session_style(Some(&slot(true, false)), None, DECK, 35.0);
    assert_eq!(focused.get("left"), Some("603px"));
    assert_eq!(focused.get("top"), Some("35px"));
    assert_eq!(focused.get("z-index"), Some("2"));
    assert_eq!(focused.get("visibility"), Some("inherit"));
    let background = terminal_session_style(Some(&slot(false, false)), None, DECK, 35.0);
    assert_eq!(background.get("z-index"), Some("1"));
}

#[test]
fn the_floated_card_covers_its_whole_rect_above_the_scrim() {
    let floated = terminal_session_style(Some(&slot(true, true)), None, DECK, 35.0);
    assert_eq!(
        floated.get("top"),
        Some("0px"),
        "the card has no strip above it"
    );
    assert_eq!(floated.get("height"), Some("800px"));
    assert_eq!(floated.get("z-index"), Some("9"));
    assert_eq!(floated.get("border-radius"), Some("var(--md-shape-md)"));
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

/// Dioxus 0.7 keeps every old inline property a new `style` string omits, so a
/// slot moving between placements keeps whatever only the old one declared —
/// a revealed pane kept the park's `pointer-events: none` and ignored the
/// wheel. Every placement therefore declares the same property set.
#[test]
fn every_slot_placement_declares_the_same_properties_so_no_state_outlives_itself() {
    let park = DeckSize { w: 597.0, h: 765.0 };
    let placements = [
        terminal_session_style(None, Some(park), DECK, 35.0),
        terminal_session_style(Some(&slot(true, false)), None, DECK, 35.0),
        terminal_session_style(Some(&slot(false, false)), None, DECK, 35.0),
        terminal_session_style(Some(&slot(true, true)), None, DECK, 35.0),
    ];
    let parked = declared_properties(&placements[0].css());
    for placement in &placements[1..] {
        assert_eq!(declared_properties(&placement.css()), parked);
    }
    assert_eq!(placements[0].get("pointer-events"), Some("none"));
    for revealed in &placements[1..] {
        assert_eq!(
            revealed.get("pointer-events"),
            Some("auto"),
            "a revealed slot takes the wheel and the pointer"
        );
    }
    assert_eq!(placements[1].get("overflow"), Some("visible"));
    assert_eq!(placements[1].get("border-radius"), Some("0"));
}
