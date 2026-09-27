//! Shared fixtures for the selection-guard cases: the pane's node identities,
//! the two selection shapes a browser produces, and the captured range a
//! capture retains.
//!
//! They live apart from the cases because a file holding both goes over the cap,
//! and because the guard's whole subject is the DIFFERENCE between the live
//! selection and the retained one — a fixture file that mixed the two would be
//! the confusion the module is about.
#![allow(dead_code)]

use roost_web_terminal::input::{
    ComposeSelection, DomNodeId, FocusOwner, LiveSelection, OwnedRow, PaneInputs, RetainedRange,
    SelectionEndpoint, SelectionGuard,
};

pub const DISPLAY: DomNodeId = DomNodeId(1);
pub const ROW: DomNodeId = DomNodeId(2);
pub const TEXT_NODE: DomNodeId = DomNodeId(3);
pub const COMPOSER: DomNodeId = DomNodeId(4);

/// The one row a pane-owned selection resolves to.
pub fn the_row() -> Vec<OwnedRow> {
    vec![OwnedRow {
        id: ROW,
        text: "v0".to_string(),
    }]
}

pub fn endpoint(offset: u32) -> SelectionEndpoint {
    SelectionEndpoint {
        node: TEXT_NODE,
        offset,
    }
}

/// A selection spanning `text` inside the pane's own row.
pub fn pane_selection(text: &str) -> LiveSelection {
    LiveSelection {
        present: true,
        collapsed: false,
        range_count: 1,
        anchor: Some(endpoint(0)),
        focus: Some(endpoint(text.chars().count() as u32)),
        text: text.to_string(),
        owned_rows: the_row(),
        ..LiveSelection::default()
    }
}

/// The retained range a capture of `text` over `rows` holds.
pub fn retained_for(text: &str, rows: Vec<OwnedRow>) -> RetainedRange {
    RetainedRange {
        display: DISPLAY,
        anchor: endpoint(0),
        focus: endpoint(text.chars().count() as u32),
        range_text: text.to_string(),
        containers_connected: true,
        rows,
    }
}

/// The retained range a capture of `text` over this pane's own row holds.
pub fn retained(text: &str) -> RetainedRange {
    retained_for(text, the_row())
}

/// A collapsed selection over `rows`: the browser's editable-focus artifact,
/// not a user selection.
pub fn caret(rows: Vec<OwnedRow>) -> LiveSelection {
    LiveSelection {
        present: true,
        collapsed: true,
        range_count: 1,
        anchor: Some(endpoint(0)),
        focus: Some(endpoint(0)),
        owned_rows: rows,
        ..LiveSelection::default()
    }
}

/// The document after a yield cleared its ranges, composer still focused.
pub fn cleared_by_yield() -> LiveSelection {
    LiveSelection {
        present: true,
        collapsed: true,
        focus_owner: Some(FocusOwner {
            node: COMPOSER,
            connected: true,
        }),
        ..LiveSelection::default()
    }
}

/// No selection anywhere on the page.
pub fn no_selection() -> LiveSelection {
    LiveSelection::default()
}

/// The live selection, with the composer's textarea holding focus.
pub fn focused_by_composer(selection: &LiveSelection) -> LiveSelection {
    LiveSelection {
        focus_owner: Some(FocusOwner {
            node: COMPOSER,
            connected: true,
        }),
        ..selection.clone()
    }
}

/// A selection whose endpoints are outside the pane's display.
pub fn foreign_selection() -> LiveSelection {
    LiveSelection {
        present: true,
        range_count: 1,
        anchor: Some(SelectionEndpoint {
            node: DomNodeId(90),
            offset: 0,
        }),
        focus: Some(SelectionEndpoint {
            node: DomNodeId(91),
            offset: 9,
        }),
        text: "elsewhere".to_string(),
        ..LiveSelection::default()
    }
}

/// The inputs one transition is decided from.
pub fn inputs<'a>(live: &'a LiveSelection, retained: &'a RetainedRange) -> PaneInputs<'a> {
    PaneInputs {
        live,
        retained: Some(retained),
        display: DISPLAY,
    }
}

/// Capture `v0` on `guard` and hand the composer the same inputs.
pub fn capture_v0(guard: &mut SelectionGuard) -> (ComposeSelection, LiveSelection, RetainedRange) {
    let mut composer = ComposeSelection::new();
    let live = pane_selection("v0");
    let held = retained("v0");
    assert!(composer.capture(guard, inputs(&live, &held)).capture);
    (composer, live, held)
}

/// A guard holding a capture whose range is yielded to the composer.
pub fn suspended_composer_pane() -> (SelectionGuard, ComposeSelection) {
    let mut guard = SelectionGuard::new();
    let (mut composer, live, held) = capture_v0(&mut guard);
    let yielded = focused_by_composer(&live);
    assert!(
        composer
            .suspend(&mut guard, inputs(&yielded, &held))
            .is_some()
    );
    (guard, composer)
}
