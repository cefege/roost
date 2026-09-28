//! The core's unhandled-sequence ring against `vte`'s own record of what it
//! dropped. `vte` reports a CSI it has no `Handler` method for only as a
//! `log::debug!` line (`[Unhandled CSI] …`); the core observes the same drop
//! with a shadow parser and a transcription of `vte`'s dispatch table. This
//! captures `vte`'s records over a corpus of sequences and fails on any
//! sequence where the two disagree, so a `vte` upgrade that moves the table
//! breaks here rather than silently mis-reporting in production.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use roost_term::{AlacrittyCore, TerminalCore};

/// Every `[Unhandled CSI]` record, tagged with the thread that logged it so a
/// concurrently running test cannot be counted.
struct DropRecords {
    seen: Arc<Mutex<Vec<ThreadId>>>,
}

impl log::Log for DropRecords {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if record.args().to_string().starts_with("[Unhandled CSI]") {
            self.seen
                .lock()
                .expect("held")
                .push(std::thread::current().id());
        }
    }

    fn flush(&self) {}
}

fn corpus() -> Vec<String> {
    let privates = ["", "?", ">", "<", "="];
    let params = [
        "",
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "14",
        "18",
        "22",
        "23",
        "1;1",
        "3;3",
        "4;0",
        "4;3",
        "4:1",
        "1;2;3;4;5",
    ];
    let intermediates = ["", "$", " ", "!"];
    let mut sequences = Vec::new();
    for private in privates {
        for param in params {
            for intermediate in intermediates {
                for final_byte in 0x40u8..=0x7e {
                    sequences.push(format!(
                        "\x1b[{private}{param}{intermediate}{}",
                        char::from(final_byte)
                    ));
                }
            }
        }
    }
    // More intermediates than `vte` collects: dispatched with `ignore` set.
    sequences.push("\x1b[?1$!p".to_string());
    sequences.push("\x1b[1 $!q".to_string());
    sequences
}

#[test]
fn the_core_reports_exactly_the_csi_vte_drops() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    log::set_boxed_logger(Box::new(DropRecords {
        seen: Arc::clone(&seen),
    }))
    .expect("the only logger this test binary installs");
    log::set_max_level(log::LevelFilter::Trace);
    let me = std::thread::current().id();
    let vte_drops = || {
        seen.lock()
            .expect("held")
            .iter()
            .filter(|id| **id == me)
            .count()
    };

    let mut core = AlacrittyCore::new(80, 24);
    let mut disagreements = Vec::new();
    let mut dropped = 0;
    for sequence in corpus() {
        let vte_before = vte_drops();
        let core_before = core.unhandled_sequences().total();
        core.write(sequence.as_bytes());
        let by_vte = vte_drops() - vte_before;
        let by_core = core.unhandled_sequences().total() - core_before;
        dropped += by_vte;
        if by_vte as u64 != by_core {
            disagreements.push(format!(
                "{sequence:?}: vte dropped {by_vte}, core logged {by_core}"
            ));
        }
    }
    assert!(
        dropped > 1000,
        "the oracle must actually see drops ({dropped}); a silent logger proves nothing"
    );
    assert!(
        disagreements.is_empty(),
        "{} disagreements, first: {:?}",
        disagreements.len(),
        disagreements.iter().take(10).collect::<Vec<_>>()
    );
}
