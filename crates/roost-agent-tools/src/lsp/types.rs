//! Ported from oh-my-pi packages/coding-agent/src/lsp/types.ts (MIT).
//! This file owns the normalized server configuration and process state.
//! Manager, document synchronization and actions share these definitions.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Child;
use tokio::sync::{Mutex, OnceCell};

use super::client::RpcClient;

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub language_ids: BTreeMap<String, String>,
    #[serde(default)]
    pub init_options: Value,
    #[serde(default)]
    pub settings: Value,
}

pub(super) struct RunningServer {
    pub rpc: RpcClient,
    pub child: Option<Child>,
    pub capabilities: Value,
    pub documents: BTreeMap<String, u64>,
    pub config: ServerConfig,
    pub last_used: Instant,
}

pub(super) struct ServerSlot {
    pub server: OnceCell<Arc<Mutex<RunningServer>>>,
    pub last_used_ms: AtomicU64,
    pub name: String,
    pub root: PathBuf,
}
