//! The pane's input and lifecycle rules: paste framing and the multiline
//! guard, the one-shot Ctrl latch, the chords the pane reserves ahead of the
//! PTY, page-lifecycle routing, and the paint feed's coalescing. Ports the rule
//! cases of `apps/web/tests/cellTerminalDocumentLifecycle.test.ts` and the input
//! paths of `apps/web/src/components/terminal/cell-terminal-input.ts`.

use roost_protocol::cell::CellGridFrame;
use roost_web::components::terminal::document_lifecycle::{
    DocumentLifecycleEvent, LifecycleAction, classify, lifecycle_action,
};
use roost_web::components::terminal::frame_feed::FrameFeed;
use roost_web::components::terminal::pane_input::{
    ChordModifiers, PasteDecision, ReservedChord, controller_data, paste_decision, reserved_chord,
    terminal_text_bytes,
};

#[test]
fn a_submission_ends_in_a_carriage_return_and_brackets_only_when_asked() {
    assert_eq!(terminal_text_bytes("ls", false, true), b"ls\r");
    assert_eq!(terminal_text_bytes("", false, true), b"\r");
    let bracketed = terminal_text_bytes("ls", true, false);
    assert!(bracketed.starts_with(b"\x1b[200~") && bracketed.ends_with(b"\x1b[201~"));
}

#[test]
fn a_multiline_paste_into_an_unbracketed_shell_is_confirmed_first() {
    assert_eq!(paste_decision("", false), PasteDecision::Ignore);
    assert_eq!(paste_decision("one line", false), PasteDecision::Send);
    assert_eq!(paste_decision("a\nb", false), PasteDecision::Send);
    assert_eq!(
        paste_decision("a\nb\nc", false),
        PasteDecision::Confirm { lines: 3 }
    );
    assert_eq!(paste_decision("a\nb\nc", true), PasteDecision::Send);
}

#[test]
fn the_ctrl_latch_controls_the_next_key_only_when_armed() {
    assert_eq!(controller_data("c", true), "\u{3}");
    assert_eq!(controller_data("c", false), "c");
}

#[test]
fn copy_paste_and_find_chords_are_reserved_but_plain_ctrl_keys_stay_the_ptys() {
    let ctrl_shift = ChordModifiers {
        ctrl: true,
        shift: true,
        ..ChordModifiers::default()
    };
    let meta = ChordModifiers {
        meta: true,
        ..ChordModifiers::default()
    };
    let ctrl = ChordModifiers {
        ctrl: true,
        ..ChordModifiers::default()
    };
    assert_eq!(reserved_chord("C", ctrl_shift), Some(ReservedChord::Copy));
    assert_eq!(reserved_chord("v", ctrl_shift), Some(ReservedChord::Paste));
    assert_eq!(
        reserved_chord("F", ctrl_shift),
        Some(ReservedChord::OpenFind)
    );
    assert_eq!(reserved_chord("f", meta), Some(ReservedChord::OpenFind));
    assert_eq!(reserved_chord("c", ctrl), None);
    assert_eq!(reserved_chord("f", ctrl), None);
    assert_eq!(
        reserved_chord("ArrowUp", ctrl_shift),
        Some(ReservedChord::PreviousPrompt)
    );
    assert_eq!(
        reserved_chord("ArrowDown", ctrl_shift),
        Some(ReservedChord::NextPrompt)
    );
    assert_eq!(reserved_chord("ArrowUp", ctrl), None);
    let alted = ChordModifiers {
        alt: true,
        ..ctrl_shift
    };
    assert_eq!(reserved_chord("c", alted), None);
}

#[test]
fn hidden_and_pagehide_park_while_a_shown_page_republishes_an_active_pane() {
    let hidden = classify("visibilitychange", false);
    assert_eq!(hidden, Some(DocumentLifecycleEvent::Hidden));
    assert_eq!(
        lifecycle_action(DocumentLifecycleEvent::Hidden, false, true),
        LifecycleAction::Park
    );
    let pagehide = classify("pagehide", true).unwrap();
    assert_eq!(
        lifecycle_action(pagehide, true, true),
        LifecycleAction::Park
    );
    for kind in ["pageshow", "resume", "visibilitychange"] {
        let edge = classify(kind, true).unwrap();
        assert_eq!(
            lifecycle_action(edge, true, true),
            LifecycleAction::Republish,
            "{kind}"
        );
    }
    // A pageshow while still hidden is a hide, not a show.
    assert_eq!(
        classify("pageshow", false),
        Some(DocumentLifecycleEvent::Hidden)
    );
    // An inactive pane that becomes visible withdraws; its other shows do nothing.
    assert_eq!(
        lifecycle_action(DocumentLifecycleEvent::Visible, true, false),
        LifecycleAction::Withdraw
    );
    assert_eq!(
        lifecycle_action(DocumentLifecycleEvent::Resume, true, false),
        LifecycleAction::Ignore
    );
    assert_eq!(classify("focus", true), None);
}

fn frame(stream: &str, epoch: &str, seq: u64) -> CellGridFrame {
    CellGridFrame {
        stream_id: stream.to_owned(),
        grid_epoch: epoch.to_owned(),
        cols: 80,
        rows: 24,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: roost_protocol::cell::MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        full: true,
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq,
    }
}

#[test]
fn replica_revisions_coalesce_into_one_paint_and_only_a_new_grid_is_a_baseline() {
    let mut feed = FrameFeed::new();
    assert!(!feed.observe_revision(0), "an empty replica owes nothing");
    assert!(
        feed.observe_revision(3),
        "a remount paints the canonical it finds"
    );
    assert!(!feed.observe_revision(3));
    assert!(feed.observe_revision(4));
    assert!(feed.paint_owed());
    assert!(feed.painted(&frame("s1", "e1", 4), 4).baseline);
    assert!(!feed.paint_owed());
    assert!(feed.observe_revision(5));
    assert!(
        !feed.painted(&frame("s1", "e1", 5), 5).baseline,
        "a continuation is output"
    );
    assert!(feed.observe_revision(6));
    assert!(
        feed.painted(&frame("s2", "e1", 6), 6).baseline,
        "a new stream re-baselines"
    );
    feed.owe_paint();
    feed.skip();
    assert!(!feed.paint_owed());
}

#[test]
fn a_pane_folds_deltas_from_what_it_painted_and_a_skip_or_park_owes_the_full() {
    let mut feed = FrameFeed::new();
    assert_eq!(feed.delta_base(), None, "a fresh mount paints the full");
    feed.observe_revision(3);
    feed.painted(&frame("s1", "e1", 3), 3);
    assert_eq!(feed.delta_base(), Some(3));
    feed.observe_revision(4);
    feed.skip();
    assert_eq!(
        feed.delta_base(),
        None,
        "the renderer never saw the skipped frame"
    );
    feed.painted(&frame("s1", "e1", 5), 5);
    feed.park();
    assert_eq!(feed.delta_base(), None, "a background pane takes the full");
}

#[test]
fn only_history_growth_on_the_same_grid_signals_a_scroll_to_the_echo() {
    let with_history = |seq: u64, total: u64| CellGridFrame {
        scrollback_total: total,
        ..frame("s1", "e1", seq)
    };
    let mut feed = FrameFeed::new();
    assert!(
        !feed.painted(&with_history(1, 5), 1).scrollback_appended,
        "a baseline is not a scroll"
    );
    assert!(
        !feed.painted(&with_history(2, 5), 2).scrollback_appended,
        "unchanged history"
    );
    assert!(
        feed.painted(&with_history(3, 7), 3).scrollback_appended,
        "coalesced rows scrolled off"
    );
    assert!(!feed.painted(&with_history(4, 7), 4).scrollback_appended);
}
