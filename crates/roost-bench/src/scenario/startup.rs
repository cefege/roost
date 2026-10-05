//! `startup`: how long a stack takes to come up, from `StartupTimings`, plus
//! the wall time of the `SessionsSpawn` RPC that opened the round's shell.

use serde_json::json;

use crate::scenario::Sample;
use crate::stack::StartupTimings;

pub fn startup_samples(timings: StartupTimings, session_spawn_rpc_ms: f64) -> Vec<Sample> {
    vec![
        Sample::new("coord_listen_ms", timings.coord_listen_ms, json!({})),
        Sample::new("worker_routable_ms", timings.worker_routable_ms, json!({})),
        Sample::new("session_spawn_rpc_ms", session_spawn_rpc_ms, json!({})),
    ]
}
