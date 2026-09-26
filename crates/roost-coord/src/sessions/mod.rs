//! The session domain: the lifecycle RPCs, the pending-spawn table, the list
//! projection, and the workspaces, tasks and MCP relays that hang off a session.
//!
//! One field on `CoordServices`, reached as `core.services.sessions`. The list
//! projection, the spawn reservation and the input lane all read the same
//! session state, so two instances would be two answers to "does this session
//! exist".
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.
//!
//! The four sub-domains are declared here rather than by the slices that fill
//! them, so workspaces, tasks and MCP can be built in parallel without three
//! agents editing this file. S4 owns the lifecycle files this module will gain
//! next and is the one slice that edits it again.

pub mod mcp;
pub mod mcp_store;
pub mod rpc_workspaces;
pub mod tasks;
pub mod workspaces;

/// The session state one coordinator process holds.
#[derive(Debug, Default)]
pub struct SessionsRuntime;

impl SessionsRuntime {
    /// A coordinator with no sessions and nothing reserved.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
