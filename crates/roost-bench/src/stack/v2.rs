//! The v2 stack: `bun` running the `main` checkout's coordinator and worker
//! entry points from source, serving that checkout's built SPA. The env tables
//! are the ones the deleted `smoke/terminal/stack-runtime.ts` and
//! `stack-worker-runtime.ts` used (`git show main:smoke/terminal/…`).

use crate::paths;
use crate::prepare::Prepared;
use crate::stack::RoundLayout;
use crate::stack::spec::{ProcessSpec, path_value, shared_coord_env, shared_worker_env};

pub fn coord_spec(layout: &RoundLayout, prepared: &Prepared) -> ProcessSpec {
    let mut env = shared_coord_env(layout);
    env.push((
        "ROOST_WEB_DIST_PATH".into(),
        path_value(&paths::v2_web_dist(&prepared.v2_root)),
    ));
    env.push(("ROOST_GIT_SHA".into(), prepared.v2_sha.clone()));
    ProcessSpec {
        program: "bun".into(),
        args: vec!["apps/coord/src/main.ts".into()],
        cwd: prepared.v2_root.clone(),
        env,
    }
}

pub fn worker_spec(layout: &RoundLayout, prepared: &Prepared, token: &str) -> ProcessSpec {
    let mut env = shared_worker_env(layout, token);
    env.push(("ROOST_KEEPER_QUIET".into(), "1".into()));
    ProcessSpec {
        program: "bun".into(),
        args: vec!["apps/worker/src/main.ts".into()],
        cwd: prepared.v2_root.clone(),
        env,
    }
}
