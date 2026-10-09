//! Kitty keyboard mode state as seen by the worker's terminal core.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_term::{RioCore, TerminalCore};

#[test]
fn set_modes_and_mode_actions_follow_the_spec() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[=3;1u");
    assert_eq!(core.kitty_keyboard_flags(), 3);
    core.write_raw(b"\x1b[=4;2u");
    assert_eq!(core.kitty_keyboard_flags(), 7);
    core.write_raw(b"\x1b[=2;3u");
    assert_eq!(core.kitty_keyboard_flags(), 5);
}

#[test]
fn push_and_pop_restore_previous_flags() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[>1u\x1b[>4u");
    assert_eq!(core.kitty_keyboard_flags(), 4);
    core.write_raw(b"\x1b[<u");
    assert_eq!(core.kitty_keyboard_flags(), 1);
    core.write_raw(b"\x1b[<u");
    assert_eq!(core.kitty_keyboard_flags(), 0);
    core.write_raw(b"\x1b[=3u\x1b[<0u");
    assert_eq!(core.kitty_keyboard_flags(), 0);
    core.write_raw(b"\x1b[=1u\x1b[>4u");
    assert_eq!(core.kitty_keyboard_flags(), 4);
    core.write_raw(b"\x1b[<u");
    assert_eq!(core.kitty_keyboard_flags(), 1);
}

#[test]
fn main_and_alternate_screens_keep_independent_stacks() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[>1u\x1b[>2u\x1b[?1049h");
    assert_eq!(core.kitty_keyboard_flags(), 0);
    core.write_raw(b"\x1b[>4u\x1b[<u");
    assert_eq!(core.kitty_keyboard_flags(), 0);
    core.write_raw(b"\x1b[?1049l");
    assert_eq!(core.kitty_keyboard_flags(), 2);
    core.write_raw(b"\x1b[?1049h");
    assert_eq!(core.kitty_keyboard_flags(), 0);
}

#[test]
fn set_modes_survive_alternate_screen_switches_independently() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[=1u\x1b[?1049h\x1b[=4u");
    assert_eq!(core.kitty_keyboard_flags(), 4);
    core.write_raw(b"\x1b[?1049l");
    assert_eq!(core.kitty_keyboard_flags(), 1);
    core.write_raw(b"\x1b[?1049h");
    assert_eq!(core.kitty_keyboard_flags(), 4);
}

#[test]
fn reset_initialization_clears_both_screen_stacks() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[>1u\x1b[>2u\x1b[?1049h\x1b[>4u\x1bc");
    assert_eq!(core.kitty_keyboard_flags(), 0);
    core.write_raw(b"\x1b[?1049l");
    assert_eq!(core.kitty_keyboard_flags(), 0);
}

#[test]
fn query_reports_current_flags() {
    let mut core = RioCore::new(80, 24);
    core.write_raw(b"\x1b[=19u\x1b[?u");
    assert_eq!(core.get_response().as_deref(), Some("\x1b[?19u"));
}
