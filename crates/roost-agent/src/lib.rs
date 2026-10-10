//! The agent harness: the conversation loop, prompts, model roles, plan mode,
//! subagents, the advisor and slash commands. The coordinator drives it through
//! the store, tool-executor and sink seams; model calls go through roost-llm.

#![forbid(unsafe_code)]
