//! What one process of the stack is: program, argv, working directory, and the
//! environment on top of `RoundLayout::base_env`. The coordinator runs the
//! release `roost` binary as `coord`, the worker as `worker` with the release
//! `roost-keeper`, and the coordinator serves the `wasm-release` web bundle
//! `prepare` built. Called by `boot.rs`, which spawns each spec.
//! Env names: roost-host `coord_config_loader.rs`, roost-worker `runtime/boot.rs`.

use std::path::PathBuf;

use crate::paths;
use crate::prepare::Prepared;
use crate::stack::RoundLayout;

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

pub fn coord_spec(layout: &RoundLayout, prepared: &Prepared) -> ProcessSpec {
    let mut env = vec![
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
        (
            "ROOST_WEB_DIST_PATH".into(),
            path_value(&paths::v3_web_dist()),
        ),
        ("ROOST_GIT_SHA".into(), prepared.roost_sha.clone()),
    ];
    push_log_level(layout, &mut env);
    ProcessSpec {
        program: paths::v3_roost(),
        args: vec!["coord".into()],
        cwd: paths::repo_root(),
        env,
    }
}

pub fn worker_spec(layout: &RoundLayout, token: &str) -> ProcessSpec {
    let mut env = vec![
        ("ROOST_COORDINATOR_URL".into(), layout.coord_url()),
        ("ROOST_BOOTSTRAP_TOKEN".into(), token.to_string()),
        ("ROOST_WORKER_LABEL".into(), "bench".into()),
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
        (
            "ROOST_KEEPER_EXECUTABLE".into(),
            path_value(&paths::v3_keeper()),
        ),
    ];
    push_log_level(layout, &mut env);
    ProcessSpec {
        program: paths::v3_roost(),
        args: vec![
            "worker".into(),
            "--coordinator-url".into(),
            layout.coord_url(),
        ],
        cwd: paths::repo_root(),
        env,
    }
}

fn push_log_level(layout: &RoundLayout, env: &mut Vec<(String, String)>) {
    if let Some(level) = &layout.child_log_level {
        env.push(("ROOST_LOG_LEVEL".into(), level.clone()));
    }
}

pub fn path_value(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}
