//! The terminal plane: the cell replica, its fences, its repair, the leases
//! that hold it open, the carriers that carry it, and the input that writes
//! through it.
//!
//! `frame_fold` is the pure rule set and the only place a full or a delta is
//! judged; `admission` is the verdict that rule produces; `session` is the state
//! it is applied to; `repair` is the conclusion a refusal draws and
//! `resync_position` what its request names; `liveness` and `session_liveness`
//! are the watchdog that notices a pane stopped painting; `token` is the
//! identity every rule is scoped to; `view` and `session_views` are the leases;
//! `routes` and `registry` are the carrier election; `input` and `router` are
//! the write path; `history` is the absolute scrollback arithmetic a pager
//! needs; `renderer_deliveries` is what a renderer folds between paints.
//!
//! Contract: `protocol/spec/terminal-stream.md` and
//! `protocol/spec/direct-terminal.md`. Every rule and its source is in
//! `docs/phase4-client-contract.md` §6, §8 and §9.

pub mod admission;
pub mod frame_counts;
pub mod frame_fold;
pub mod history;
pub mod history_backfill;
pub mod input;
pub mod liveness;
pub mod renderer_deliveries;
pub mod repair;
pub mod resync_position;
pub mod routes;
pub mod session;
pub mod session_liveness;
pub mod session_views;
pub mod session_wire;
pub mod smoke_faults;
pub mod token;
pub mod view;

pub use admission::{Admission, ViewStateAdmission};
pub use frame_fold::{
    FoldTarget, FrameFoldFailure, FrameFoldOutcome, decode_wire_frame, fold,
    full_follows_canonical, valid_full,
};
pub use history::{HistoryRange, HistoryScrollTarget};
pub use input::{
    InputLane, InputOutcome, InputPhase, InputRefusal, InputRouter, PendingInput, TerminalFence,
};
pub use liveness::{ForegroundLiveness, RepairOutcome, StallAction};
pub use repair::RepairLatch;
pub use resync_position::ResyncPosition;
pub use routes::{
    CancelledCandidate, DirectCarrier, PromotionCandidate, PromotionRefusal, ProspectiveView,
    RouteRegistry, SessionRoute,
};
pub use session::TerminalSession;
pub use token::{TerminalToken, TerminalTransport, token_matches};
pub use view::{TerminalView, ViewIntent, ViewStateResult};
