//! Browser UI reports, legacy UI commands, and the acknowledged layout apply.
//!
//! The four Connect methods are `ui_report_state`, `ui_list_states`,
//! `ui_dispatch` and `ui_apply_layout`; `rpc` mounts them and the Sync feed
//! reads the same state. Two process singletons live here -- the retained
//! reports and the reserved applies -- because v2 kept both as module-level
//! `Map`s in a codebase that had no other option, and `services.rs`'s own rule
//! is that nothing in this crate reaches for a global.

pub mod fence;
pub mod identity_rate;
pub mod layout_apply;
pub mod layout_proto;
pub mod legacy_command;
pub mod limits;
pub mod rejected_reason;
pub mod rpc;
pub mod state_owner;

use std::sync::Arc;

use crate::ui_state::layout_apply::UiLayoutApplyOwner;
use crate::ui_state::state_owner::UiStateOwner;

/// The UI state one coordinator process holds.
///
/// A field on `CoordServices` (`ui_state`), reached as `core.services.ui_state`,
/// so the RPC that reserves an apply and the Sync ingress that settles it are
/// looking at one table. Two instances would be two answers to "is that tab
/// still there", and only one of them would ever be published to.
#[derive(Debug, Clone)]
pub struct UiStateRuntime {
    states: UiStateOwner,
    layout_applies: UiLayoutApplyOwner,
}

impl UiStateRuntime {
    /// A runtime over the real clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::serve::now_ms))
    }

    /// A runtime whose clock the caller supplies, so a test never waits for
    /// this one to tick.
    #[must_use]
    pub fn with_clock(now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            states: UiStateOwner::with_clock(Arc::clone(&now_ms)),
            layout_applies: UiLayoutApplyOwner::with_clock(now_ms),
        }
    }

    /// The retained tab reports.
    #[must_use]
    pub fn states(&self) -> &UiStateOwner {
        &self.states
    }

    /// The live layout-apply targets and their reservations.
    #[must_use]
    pub fn layout_applies(&self) -> &UiLayoutApplyOwner {
        &self.layout_applies
    }
}

impl Default for UiStateRuntime {
    fn default() -> Self {
        Self::new()
    }
}
