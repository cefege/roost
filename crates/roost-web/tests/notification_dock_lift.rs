//! Where the notification dock sits relative to the bottom chrome.
//!
//! The dock's bottom edge IS a card's bottom edge, so this is the one decision
//! that decides whether a toast is readable or buried under a composer. Three
//! cases, one per bottom chrome, and the compact one has to move with a
//! measurement because a composer grows.
//!
//! Native, because the whole decision is a function of values the dock already
//! holds — the same separation `notificationDockLift.ts` keeps in v2, and the
//! reason this is a sibling binary rather than an inline module.

use roost_web::components::layout::notification_dock_lift::notification_dock_lift;
use roost_web::components::layout::shell_style::ComposerGeometry;

#[test]
fn an_open_composer_lifts_the_dock_by_what_it_measured_not_by_a_constant() {
    // The dock's bottom edge is where a card's bottom edge lands, and the
    // composer is the only bottom chrome whose height MOVES. A resting-row
    // constant here would put a toast under a two-line draft.
    let composer = ComposerGeometry {
        active: true,
        height_px: 132.0,
    };
    assert_eq!(
        notification_dock_lift(composer, true),
        "calc(var(--term-chat-dock-offset) + 132px + var(--md-space-2))"
    );
    assert_ne!(
        notification_dock_lift(composer, true),
        notification_dock_lift(
            ComposerGeometry {
                active: true,
                height_px: 48.0
            },
            true
        ),
        "a taller composer has to move the dock, or the two overlap"
    );
}

#[test]
fn a_compact_shell_with_no_composer_clears_the_dock_offset_alone() {
    // Compact rides on the composer's own offset, which already folds the safe
    // area and the soft-keyboard inset. Adding a status bar to that would push
    // the dock off a phone screen entirely.
    assert_eq!(
        notification_dock_lift(
            ComposerGeometry {
                active: false,
                height_px: 0.0
            },
            true
        ),
        "var(--term-chat-dock-offset)"
    );
}

#[test]
fn a_pointer_shell_clears_the_status_bar_and_the_resting_composer_row() {
    // One constant for the pointer case, so the dock never depends on route or
    // pane count — a dock that moved with the pane count would jump every time
    // a pane was split.
    let lift = notification_dock_lift(
        ComposerGeometry {
            active: false,
            height_px: 0.0,
        },
        false,
    );
    assert_eq!(
        lift,
        "calc(var(--workbench-statusbar-height) + var(--term-chat-rest-height) \
         + max(var(--kb-offset), 0px) + var(--md-space-4))"
    );
}

#[test]
fn a_leftover_height_from_a_disposing_composer_does_not_lift_the_dock() {
    // The active flag is what says a composer is really there. A height left in
    // the slot by a dock that is on its way out must not keep the notifications
    // clear of a composer that is not there.
    let resting = ComposerGeometry {
        active: false,
        height_px: 0.0,
    };
    let disposing = ComposerGeometry {
        active: false,
        height_px: 132.0,
    };
    for compact in [true, false] {
        assert_eq!(
            notification_dock_lift(disposing, compact),
            notification_dock_lift(resting, compact)
        );
    }
}
