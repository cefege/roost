//! The frames a route serves AFTER it is elected.
//!
//! Split from the candidate half beside it because the two are the two ends of
//! one promotion, and the failure mode of getting them confused is invisible: a
//! candidate with a half-built baseline must not be able to paint
//! (`protocol/spec/direct-terminal.md:27`), and an ELECTED route that quietly
//! stopped folding looks exactly like a healthy one — the baseline that arrived
//! with the swap painted, and then nothing ever changed again, with the header
//! still claiming a live carrier.
//!
//! Contract: `protocol/spec/direct-terminal.md`; the reasons are in
//! `docs/phase4-client-contract.md` §8.

use crate::store::Store;
use crate::store::frames_revision::PaintedMark;
use crate::terminal::session::{Admission, TerminalSession};
use crate::terminal::token::TerminalToken;

/// Fold one direct-carrier cell frame into the session's ELECTED replica.
///
/// The route is already this token's, so the canonical replica IS the destination
/// — the same object the commit installed, fenced to the generation the frames
/// arrive on. No promotion happens here and none is needed: the fences already
/// ran, and a frame that arrives on a route the registry does not present is
/// refused by the replica's own generation check.
pub(super) fn fold_into_elected<F>(
    store: &mut Store,
    session_id: &str,
    token: &TerminalToken,
    fold: F,
) where
    F: FnOnce(&mut TerminalSession) -> Admission,
{
    let (before, after) = {
        let Some(replica) = store.terminal_mut_if_present(session_id) else {
            return;
        };
        replica.bind_generation(token);
        let before = PaintedMark::of(replica);
        let _ = fold(replica);
        (before, PaintedMark::of(replica))
    };
    store.note_fold(before, after);
}
