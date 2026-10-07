//! The guard on the kitty keyboard stack cap and oldest-entry eviction.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::{Config, Term, TermMode, test::TermSize};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

#[test]
fn a_full_keyboard_stack_evicts_its_oldest_entry_without_touching_titles() {
    let config = Config {
        kitty_keyboard: true,
        ..Config::default()
    };
    let mut term = Term::new(config, &TermSize::new(80, 24), VoidListener);
    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(&mut term, b"\x1b[>0u");
    let sequences = "\x1b[>1u".repeat(4096);
    processor.advance(&mut term, sequences.as_bytes());

    assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    processor.advance(&mut term, b"\x1b[<4095u");
    assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    processor.advance(&mut term, b"\x1b[<u");
    assert!(!term.mode().contains(TermMode::KITTY_KEYBOARD_PROTOCOL));
}
