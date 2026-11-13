//! Agent chat conversations and their loaded transcript replicas.
//! Hydration and Sync frames enter here; UI intents own snapshot replacement.

pub mod export;
mod host_inbox;
mod intent;
mod omp_actions;
mod presentation_clocks;
mod state;

pub use host_inbox::{
    MAX_CANCELLED_HOST_SERVICES, MAX_HOST_SERVICE_VALUE_BYTES, MAX_PENDING_ACTION_RECEIPTS,
    MAX_PENDING_HOST_SERVICES, OmpHostInbox, OmpHostService, OmpHostServiceKind,
    OmpUiActionReceipt,
};
pub use intent::AgentChatIntent;
pub use omp_actions::{
    MAX_PENDING_OMP_ACTIONS, OmpActionClass, OmpActionIdentity, OmpActionQueue, OmpQueuedAction,
    OmpReceiptOutcome, OmpSubmit, classify_action,
};
pub use presentation_clocks::{OmpPresentationClocks, OmpTimerKind, OmpTimerReading};
pub use state::{AgentChatState, LoadedTranscript};
