//! Agent chat conversations and their loaded transcript replicas.
//! Hydration and Sync frames enter here; UI intents own snapshot replacement.

mod intent;
mod state;

pub use intent::AgentChatIntent;
pub use state::{AgentChatState, LoadedTranscript};
