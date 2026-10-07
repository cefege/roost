use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

struct EventCapture(Arc<Mutex<Vec<String>>>);

impl EventListener for EventCapture {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            if let Ok(mut events) = self.0.lock() {
                events.push(text);
            }
        }
    }
}

#[test]
fn keyboard_query_reports_flags_after_a_set_operation() {
    let config = Config {
        kitty_keyboard: true,
        ..Config::default()
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let listener = EventCapture(Arc::clone(&events));
    let mut term = Term::new(config, &TermSize::new(80, 24), listener);
    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(&mut term, b"\x1b[>1u\x1b[=19;1u\x1b[?u");

    let events = events.lock().expect("event capture lock");
    assert_eq!(events.as_slice(), ["\x1b[?19u"]);
}
