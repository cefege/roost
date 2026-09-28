// The fixtures the deploy tests share: the v2 catch-up test's worker and SHAs,
// a scratch service directory for the rollout probe, a starter that records
// what it was asked to deploy, and a real subprocess job with a scripted
// transcript.
//
// `unwrap` and `expect` are denied outside `#[cfg(test)]`, and a shared test
// fixture is its own crate rather than a module of one, so the exemption has to
// be stated here. Every panic below names a value the fixture just built.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::deploy::catchup_decision::CatchUpWorkerRow;
use roost_coord::deploy::job_process::spawn_deploy_process;
use roost_coord::deploy::jobs::{DeployJournal, DeployStreamMsg};
use roost_coord::deploy::output_stream::{DeployOutput, SubscriberQueueOverflow};
use roost_coord::deploy::start::DeployStartResult;
use roost_host::MapEnv;

/// The fleet's release: the coordinator's own SHA.
pub const FLEET_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// A release the fleet has moved past.
pub const BEHIND_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The behind worker's reachable address.
pub const HOST: &str = "m1-us.tailnet.ts.net";
/// A well-formed job id no journal holds.
pub const UNHELD_JOB_ID: &str = "00000000-0000-4000-8000-000000000001";

/// v2's `BEHIND_WORKER`: POSIX, reachable, one release behind.
#[must_use]
pub fn behind_worker() -> CatchUpWorkerRow {
    CatchUpWorkerRow {
        fp: "f".repeat(64),
        os: Some("linux".to_owned()),
        label: "m1-us".to_owned(),
        reachable_addr: Some(HOST.to_owned()),
        git_sha: Some(BEHIND_SHA.to_owned()),
        keeper_runtime_json: None,
    }
}

/// A service directory of the test's own, removed when it drops.
pub struct ScratchServiceDir {
    root: PathBuf,
}

impl ScratchServiceDir {
    pub fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-deploy-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch service directory");
        Self { root }
    }

    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// An environment that resolves the service directory here.
    #[must_use]
    pub fn env(&self) -> MapEnv {
        MapEnv::new().with(
            roost_host::SERVICE_DIR_ENV,
            self.root.to_str().expect("a UTF-8 scratch path"),
        )
    }
}

impl Drop for ScratchServiceDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A deploy starter that records every host it was asked for and answers the
/// same way each time.
pub struct RecordingStarter {
    started: Mutex<Vec<String>>,
    answer: Mutex<DeployStartResult>,
}

impl RecordingStarter {
    /// A starter whose job exists under `job_id`.
    pub fn started(job_id: &str) -> Self {
        Self::answering(DeployStartResult::Started {
            job_id: job_id.to_owned(),
        })
    }

    /// A starter that cannot open a job.
    pub fn refusing() -> Self {
        Self::answering(DeployStartResult::Refused {
            error: "no coordinator URL".to_owned(),
        })
    }

    fn answering(answer: DeployStartResult) -> Self {
        Self {
            started: Mutex::new(Vec::new()),
            answer: Mutex::new(answer),
        }
    }

    /// Answer the next starts with a job under `job_id`.
    pub fn answer_with(&self, job_id: &str) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = DeployStartResult::Started {
            job_id: job_id.to_owned(),
        };
    }

    /// The starter's call shape.
    pub fn start(&self, host: &str, _release_sha: &str) -> DeployStartResult {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(host.to_owned());
        self.answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The starter as the `Fn` a catch-up option takes, borrowing this record.
    pub fn deploy_fn(&self) -> impl Fn(&str, &str) -> DeployStartResult + Sync + '_ {
        move |host, release_sha| self.start(host, release_sha)
    }

    /// Every host started so far.
    #[must_use]
    pub fn hosts(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

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
    drain(roost_coord::deploy::output_stream::open_deploy_output(journal, job_id)).await;
}

/// A line message.
#[must_use]
pub fn line(text: &str) -> Result<DeployStreamMsg, SubscriberQueueOverflow> {
    Ok(DeployStreamMsg::Line(text.to_owned()))
}

/// A done message.
#[must_use]
pub fn done(exit: Option<i32>, error: Option<&str>) -> Result<DeployStreamMsg, SubscriberQueueOverflow> {
    Ok(DeployStreamMsg::Done {
        exit,
        error: error.map(str::to_owned),
    })
}
