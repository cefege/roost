//! Observing the CSI sequences `vte` drops, which alacritty cannot report.
//! [`super::AlacrittyCore`] feeds every byte it parses through the shadow here
//! as well; the ring it fills is what `TerminalCore::unhandled_sequences`
//! answers. Ports the logging half of v2's `@wterm/core` debug ring
//! (`apps/worker/src/session/session-unhandled-seq.ts` reads it).
//!
//! WHERE THE DROP HAPPENS. `vte` 0.15 `ansi.rs` `Performer::csi_dispatch` routes
//! a sequence it has no `Handler` method for to a local `unhandled!` macro that
//! only writes a `log::debug!` line; alacritty never sees it. So the shadow runs
//! a second `vte::Parser` — the same state machine, so tokenization cannot
//! drift — whose `csi_dispatch` answers "would `Performer` have dropped this?"
//! with [`dispatched_by_vte`], a transcription of that match.
//!
//! THE COST TRADE, stated because it is a choice: every byte crosses a second
//! byte-state-machine pass (no allocation for CSI; `vte` still buffers OSC
//! payloads internally), against vendoring `vte` to add a hook at the drop
//! site. The transcription is pinned to `vte`'s real behaviour by
//! `tests/unhandled_csi_parity.rs`, which captures `vte`'s own
//! `[Unhandled CSI]` log records over a corpus and fails on any disagreement —
//! so a `vte` upgrade that moves the table breaks a test, not a diagnostic.

use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::unhandled::{UNHANDLED_PARAMS_RECORDED, UnhandledSequence, UnhandledSequenceRing};

/// The shadow parser and the ring it fills. One per core, for the core's life:
/// its parser state is the stream's, so re-creating it would split a sequence.
#[derive(Default)]
pub(crate) struct CsiShadow {
    parser: Parser,
    classifier: DropClassifier,
}

impl CsiShadow {
    /// Advance the shadow over the same bytes the core just parsed.
    pub(crate) fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.classifier, bytes);
    }

    pub(crate) fn ring(&self) -> &UnhandledSequenceRing {
        &self.classifier.ring
    }
}

/// The `Perform` that does nothing but classify completed CSI sequences.
#[derive(Default)]
struct DropClassifier {
    ring: UnhandledSequenceRing,
}

impl Perform for DropClassifier {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if dispatched_by_vte(params, intermediates, ignore, action) {
            return;
        }
        let mut recorded = [0u16; UNHANDLED_PARAMS_RECORDED];
        for (slot, param) in recorded.iter_mut().zip(params.iter()) {
            *slot = param.first().copied().unwrap_or_default();
        }
        let private = match intermediates.first() {
            Some(&marker) if (b'<'..=b'?').contains(&marker) => marker,
            _ => 0,
        };
        self.ring.record(UnhandledSequence {
            final_byte: u8::try_from(action).unwrap_or(b'?'),
            private,
            param_count: u16::try_from(params.len()).unwrap_or(u16::MAX),
            params: recorded,
        });
    }
}

/// Whether `vte` 0.15's `Performer::csi_dispatch` hands this sequence to a
/// `Handler` method rather than to its `unhandled!` macro.
///
/// A line-for-line transcription of that match, including its parameter
/// guards and the order in which they consume parameters: a guard that fails
/// falls through to the catch-all, which is the drop. `next_param_or` is the
/// same closure `vte` uses — a zero or absent parameter reads as the default.
fn dispatched_by_vte(params: &Params, intermediates: &[u8], ignore: bool, action: char) -> bool {
    if ignore || intermediates.len() > 2 {
        return false;
    }
    let mut params_iter = params.iter();
    let mut next_param_or = |default: u16| match params_iter.next() {
        Some(&[param, ..]) if param != 0 => param,
        _ => default,
    };
    match (action, intermediates) {
        (
            '@' | 'A' | 'B' | 'e' | 'b' | 'C' | 'a' | 'D' | 'd' | 'E' | 'F' | 'G' | '`' | 'H' | 'f'
            | 'h' | 'I' | 'L' | 'l' | 'M' | 'm' | 'n' | 'P' | 'r' | 'S' | 's' | 'T' | 'u' | 'X'
            | 'Z',
            [],
        ) => true,
        ('c', _) => next_param_or(0) == 0,
        ('W', [b'?']) => next_param_or(0) == 5,
        ('g', []) => matches!(next_param_or(0), 0 | 3),
        ('h' | 'l', [b'?']) => true,
        ('J', []) => matches!(next_param_or(0), 0..=3),
        ('K', []) => matches!(next_param_or(0), 0..=2),
        ('k', [b' ']) => matches!(next_param_or(0), 0..=2) && matches!(next_param_or(0), 0..=2),
        ('m', [b'>']) => next_param_or(1) == 4 && matches!(next_param_or(0), 0..=2),
        ('m', [b'?']) => params.iter().next() == Some(&[4]),
        ('p', [b'$'] | [b'?', b'$']) => true,
        ('q', [b' ']) => matches!(next_param_or(0), 0..=6),
        ('t', []) => matches!(next_param_or(1), 14 | 18 | 22 | 23),
        ('u', [b'?' | b'=' | b'>' | b'<']) => true,
        _ => false,
    }
}
