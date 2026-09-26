//! The agent status hub: one table of what every agent in the fleet is doing,
//! keyed so a stale report cannot overwrite a fresh one. Owned by the agent
//! slice; the module declaration lives in `agents/mod.rs`.
