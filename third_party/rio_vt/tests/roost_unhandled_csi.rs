//! The guard on R6: a CSI the performer drops reaches the listener's
//! `unhandled_csi` hook once; a handled one never does. See
//! `ROOST-PATCHES.md`.

use std::cell::RefCell;
use std::rc::Rc;

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{EventListener, WindowId};
use rio_vt::performer::handler::Processor;

type Seen = Rc<RefCell<Vec<(u8, u8, u16, Vec<u16>)>>>;

#[derive(Clone, Default)]
struct Recorder(Seen);

impl EventListener for Recorder {
    fn unhandled_csi(
        &self,
        final_byte: u8,
        private: u8,
        param_count: u16,
        params: &[u16],
    ) {
        self.0
            .borrow_mut()
            .push((final_byte, private, param_count, params.to_vec()));
    }
}

#[test]
fn a_dropped_csi_reaches_the_hook_and_a_handled_one_does_not() {
    let recorder = Recorder::default();
    let mut term = Crosswords::new(
        CrosswordsSize::new(80, 24),
        CursorShape::Block,
        recorder.clone(),
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();

    parser.advance(&mut term, b"\x1b[6n");
    assert!(recorder.0.borrow().is_empty(), "CPR is handled");

    parser.advance(&mut term, b"\x1b[?7;9;9Z");
    assert_eq!(
        *recorder.0.borrow(),
        vec![(b'Z', b'?', 3, vec![7, 9, 9, 0])],
        "the dropped sequence is reported once"
    );
}
