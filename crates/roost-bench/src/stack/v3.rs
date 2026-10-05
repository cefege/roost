//! The v3 stack: the release `roost` binary as `coord` and as `worker`, with
//! the release `roost-keeper` and the `wasm-release` web bundle `prepare` built.
//! Env names: roost-host `coord_config_loader.rs`, roost-worker `runtime/boot.rs`.

use crate::paths;
use crate::prepare::Prepared;
use crate::stack::RoundLayout;
use crate::stack::spec::{ProcessSpec, path_value, shared_coord_env, shared_worker_env};

pub fn coord_spec(layout: &RoundLayout, prepared: &Prepared) -> ProcessSpec {
    let mut env = shared_coord_env(layout);
    env.push((
        "ROOST_WEB_DIST_PATH".into(),
        path_value(&paths::v3_web_dist()),
    ));
    env.push(("ROOST_GIT_SHA".into(), prepared.v3_sha.clone()));
    ProcessSpec {
        program: paths::v3_roost(),
        args: vec!["coord".into()],
        cwd: paths::repo_root(),
        env,
    }
}

pub fn worker_spec(layout: &RoundLayout, token: &str) -> ProcessSpec {
    let mut env = shared_worker_env(layout, token);
    env.push((
        "ROOST_KEEPER_EXECUTABLE".into(),
        path_value(&paths::v3_keeper()),
    ));
    // A scratch stack must not rewrite the agent loaders an installed worker owns.
    env.push(("ROOST_SKIP_AGENT_INTEGRATIONS".into(), "1".into()));
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
