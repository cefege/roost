//! The keeper's JSON frames: everything a spawn and its acknowledgement carry.
//! Owned by the keeper and the worker's client.
//!
//! The contract is `protocol/spec/keeper.md`; the envelope and the tag table
//! live in [`crate::codec`]. These are separated from the binary payloads
//! because they change for a different reason: a JSON shape moves when a
//! consumer needs a field, and a binary payload moves when the protocol needs
//! a boundary.

use serde::{Deserialize, Serialize};

/// The command and environment a spawned PTY runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub cwd: Option<String>,
}

impl ShellSpec {
    /// The spec as a login shell of the current user, which is what a channel
    /// with no recorded command opens.
    pub fn default_login(shell: impl Into<String>) -> Self {
        let shell = shell.into();
        Self {
            program: shell.clone(),
            args: vec!["-l".to_string()],
            env: vec![("SHELL".to_string(), shell)],
            cwd: None,
        }
    }
}

/// `Spawn`: open a PTY with this geometry running this command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub channel_id: u16,
    pub cols: u16,
    pub rows: u16,
    pub shell_spec: ShellSpec,
}

/// `SpawnAck`: the PTY exists and this is its process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnAck {
    pub channel_id: u16,
    pub pid: u32,
}

/// `SpawnErr`: the PTY could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnErr {
    pub channel_id: u16,
    pub error: String,
}

/// `Exit`: the child ended. `exit_code` is `None` when it was killed by a
/// signal, which is not the same as a nonzero exit and a client may care.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitFrame {
    pub exit_code: Option<i32>,
}

/// `ListChannelsResp`: the channels this keeper still owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListChannelsResp {
    pub channels: Vec<ChannelBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelBinding {
    pub channel_id: u16,
    pub pid: u32,
}
