// The fixtures the deploy tests share: the fleet SHA and a reachable host, and a
// real subprocess job with a scripted transcript.
//
// `unwrap` and `expect` are denied outside `#[cfg(test)]`, and a shared test
// fixture is its own crate rather than a module of one, so the exemption has to
// be stated here. Every panic below names a value the fixture just built.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::sync::Arc;

use roost_coord::deploy::job_process::spawn_deploy_process;
use roost_coord::deploy::jobs::{DeployJournal, DeployStreamMsg};
use roost_coord::deploy::output_stream::{DeployOutput, SubscriberQueueOverflow};

/// The fleet's release: the coordinator's own SHA.
pub const FLEET_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The reachable address of a worker the tests name.
pub const HOST: &str = "m1-us.tailnet.ts.net";
/// A well-formed job id no journal holds.
pub const UNHELD_JOB_ID: &str = "00000000-0000-4000-8000-000000000001";

/// Open a job for `host` whose subprocess is `sh -c script`, killed if the test
/// ends first. Returns the job id.
pub fn scripted_job(journal: &Arc<DeployJournal>, host: &str, script: &str) -> String {
    let job = journal.open_job(host).expect("a job id");
    let job_id = job.job_id().to_owned();
    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(script).kill_on_drop(true);
    spawn_deploy_process(journal, job, command);
    job_id
}

/// Read one output to its end.
pub async fn drain(
    mut output: DeployOutput,
) -> Vec<Result<DeployStreamMsg, SubscriberQueueOverflow>> {
    let mut messages = Vec::new();
    while let Some(message) = output.next_message().await {
        messages.push(message);
    }
    messages
}

/// Wait for a job to finish by reading it to its end.
pub async fn settled(journal: &DeployJournal, job_id: &str) {
    drain(roost_coord::deploy::output_stream::open_deploy_output(
        journal, job_id,
    ))
    .await;
}

/// A line message.
pub fn line(text: &str) -> Result<DeployStreamMsg, SubscriberQueueOverflow> {
    Ok(DeployStreamMsg::Line(text.to_owned()))
}

/// A done message.
pub fn done(
    exit: Option<i32>,
    error: Option<&str>,
) -> Result<DeployStreamMsg, SubscriberQueueOverflow> {
    Ok(DeployStreamMsg::Done {
        exit,
        error: error.map(str::to_owned),
    })
}
