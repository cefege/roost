//! What one stack process is: program, argv, working directory, and the
//! stack-specific environment on top of `RoundLayout::base_env`. `v2.rs` and
//! `v3.rs` fill it in; `boot.rs` spawns it.

use std::path::PathBuf;

use crate::prepare::Prepared;
use crate::stack::{RoundLayout, StackId, v2, v3};

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl ProcessSpec {
    pub fn describe(&self) -> String {
        std::iter::once(self.program.display().to_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The coordinator environment both stacks share: the same keys, the same
/// values, so neither stack is measured under a different policy.
pub fn shared_coord_env(layout: &RoundLayout) -> Vec<(String, String)> {
    vec![
        (
            "ROOST_COORDINATOR_BIND".into(),
            format!("127.0.0.1:{}", layout.coord_port),
        ),
        ("ROOST_COORDINATOR_DB".into(), path_value(&layout.coord_db)),
        (
            "ROOST_COORDINATOR_AUTHORIZED_KEYS".into(),
            path_value(&layout.authorized_keys),
        ),
        ("ROOST_CORS_ALLOWED_ORIGINS".into(), layout.door_origin()),
        ("ROOST_TERMINAL_PEER_ENABLED".into(), "0".into()),
        ("ROOST_RELAXED_CSP".into(), "1".into()),
        ("ROOST_TRUST_PROXY".into(), "0".into()),
    ]
}

/// The worker environment both stacks share.
pub fn shared_worker_env(layout: &RoundLayout, token: &str) -> Vec<(String, String)> {
    vec![
        ("ROOST_COORDINATOR_URL".into(), layout.coord_url()),
        ("ROOST_BOOTSTRAP_TOKEN".into(), token.to_string()),
        (
            "ROOST_WORKER_LABEL".into(),
            format!("bench-{}", layout.stack.as_str()),
        ),
        (
            "ROOST_WORKER_DATA_DIR".into(),
            path_value(&layout.worker_data),
        ),
        (
            "ROOST_WORKER_KEY_PATH".into(),
            path_value(&layout.worker_data.join("worker.key")),
        ),
        (
            "ROOST_WORKER_LOCAL_UI_BIND".into(),
            format!("127.0.0.1:{}", layout.door_port),
        ),
        ("ROOST_TERMINAL_PEER_ENABLED".into(), "0".into()),
        ("SHELL".into(), "/bin/bash".into()),
    ]
}

pub fn coord_spec(layout: &RoundLayout, prepared: &Prepared) -> ProcessSpec {
    match layout.stack {
        StackId::V2 => v2::coord_spec(layout, prepared),
        StackId::V3 => v3::coord_spec(layout, prepared),
    }
}

pub fn worker_spec(layout: &RoundLayout, prepared: &Prepared, token: &str) -> ProcessSpec {
    match layout.stack {
        StackId::V2 => v2::worker_spec(layout, prepared, token),
        StackId::V3 => v3::worker_spec(layout, token),
    }
}

pub fn path_value(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}
