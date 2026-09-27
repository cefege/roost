//! Browser-tab UI state: the acknowledged layout apply, and the fence around
//! it.
//!
//! Ported from `apps/web/src/client/ui-state/uiLayoutApplyCore.ts` (`apply`)
//! and the caller side of `apps/coord/src/ui-state/ui-layout-apply-owner.ts`
//! (`fence`). Both halves are here because they are one contract: an apply
//! names an exact tab and socket on the way in, and the answer to that apply is
//! read against the same fence on the way out.

pub mod apply;
pub mod fence;

pub use apply::{
    ExactApplyTarget, LAYOUT_APPLY_DIAGNOSTIC_CORRELATION_MAX_CODE_POINTS,
    LAYOUT_APPLY_SETTLED_EVENT, LayoutApplyCommand, LayoutApplyConsumption, LayoutApplyContext,
    LayoutApplyExecution, LayoutApplyFolder, LayoutApplyOutcome, LayoutApplyRejection,
    LayoutApplyResult, LayoutApplySettlementDiagnostic, execute_targeted_layout_apply,
    reject_layout_apply_without_bridge,
};
pub use fence::{
    LAYOUT_APPLY_REFETCH_REASON, LayoutApplyAnswer, LayoutApplyRecovery, LayoutApplyTarget,
    PendingLayoutApplies, PendingLayoutApply, ReportedTab, compose_layout_apply,
    settle_layout_apply,
};
