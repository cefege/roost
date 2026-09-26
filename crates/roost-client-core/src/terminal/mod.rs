//! The terminal plane: the cell replica, its fences, its repair, the leases
//! that hold it open, the carriers that carry it, and the input that writes
//! through it.
//!
//! `frame_fold` is the pure rule set and the only place a full or a delta is
//! judged; `session` is the state that rule is applied to; `repair` is the
//! conclusion a refusal draws; `token` is the identity every rule is scoped to;
//! `view` and `session_views` are the leases; `routes` and `registry` are the
//! carrier election; `input` and `router` are the write path; `history` is the
//! absolute scrollback arithmetic a pager needs.
//!
//! Contract: `protocol/spec/terminal-stream.md` and
//! `protocol/spec/direct-terminal.md`. Every rule and its source is in
//! `docs/phase4-client-contract.md` §6, §8 and §9.

pub mod frame_fold;
pub mod history;
pub mod input;
pub mod repair;
pub mod routes;
pub mod session;
pub mod session_views;
pub mod token;
pub mod view;

pub use frame_fold::{
    FoldTarget, FrameFoldFailure, FrameFoldOutcome, decode_wire_frame, fold,
    full_follows_canonical, valid_full,
};
pub use history::{HistoryRange, HistoryScrollTarget};
pub use input::{
    InputLane, InputOutcome, InputPhase, InputRefusal, InputRouter, PendingInput, TerminalFence,
};
pub use repair::RepairLatch;
pub use routes::{
    DirectCarrier, PromotionCandidate, PromotionRefusal, RouteRegistry, SessionRoute,
};
pub use session::{Admission, TerminalSession, ViewStateAdmission};
pub use token::{TerminalToken, TerminalTransport, token_matches};
pub use view::{TerminalView, ViewIntent, ViewStateResult};
