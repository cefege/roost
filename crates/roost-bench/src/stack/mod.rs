//! Booting one stack (coordinator + worker) in isolation: free loopback ports,
//! a fresh `HOME`, a scrubbed environment, and a per-round directory. Called by
//! `run`; `spec` says WHAT each stack runs, `boot` says HOW it is started and
//! stopped.

mod boot;
mod ports;
mod spec;
mod v2;
mod v3;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::BenchError;

pub use boot::{StackHandle, StartupTimings, boot_stack, sweep_marked};

/// The two stacks under comparison.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum StackId {
    /// Branch `main`: Bun + TypeScript + SolidJS.
    V2,
    /// This tree: Rust + Dioxus/wasm.
    V3,
}

impl StackId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::V3 => "v3",
        }
    }
}

/// Every path and port one (stack, round) owns.
#[derive(Debug, Clone)]
pub struct RoundLayout {
    pub stack: StackId,
    pub round: u32,
    pub root: PathBuf,
    /// `HOME` and `TMPDIR` for every child.
    pub home: PathBuf,
    pub coord_db: PathBuf,
    pub coord_log: PathBuf,
    pub worker_log: PathBuf,
    pub worker_data: PathBuf,
    pub authorized_keys: PathBuf,
    pub chrome_profile: PathBuf,
    pub coord_port: u16,
    pub door_port: u16,
    /// The `ROOST_BENCH_RUN` value the sampler finds this round's processes by.
    pub marker: String,
}

impl RoundLayout {
    pub fn create(
        run_dir: &Path,
        run_id: &str,
        stack: StackId,
        round: u32,
    ) -> Result<Self, BenchError> {
        let root = run_dir.join(stack.as_str()).join(format!("round{round}"));
        let home = root.join("home");
        let worker_data = root.join("worker-data");
        let chrome_profile = root.join("chrome-profile");
        for dir in [&home, &worker_data, &chrome_profile] {
            std::fs::create_dir_all(dir)
                .map_err(|error| BenchError::io(format!("creating {}", dir.display()), error))?;
        }
        let authorized_keys = root.join("authorized_keys.roost");
        std::fs::write(&authorized_keys, "").map_err(|error| {
            BenchError::io(format!("creating {}", authorized_keys.display()), error)
        })?;
        Ok(Self {
            stack,
            round,
            coord_db: root.join("coord.db"),
            coord_log: root.join("coord.log"),
            worker_log: root.join("worker.log"),
            authorized_keys,
            coord_port: ports::free_loopback_port()?,
            door_port: ports::free_loopback_port()?,
            marker: format!("{run_id}/{}/{round}", stack.as_str()),
            root,
            home,
            worker_data,
            chrome_profile,
        })
    }

    pub fn coord_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.coord_port)
    }

    pub fn door_origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.door_port)
    }

    /// The environment every child of this round starts from: the harness's
    /// own, minus every `ROOST_` key, plus an isolated `HOME` and the marker.
    pub fn base_env(&self) -> Vec<(String, String)> {
        let home = self.home.to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> = std::env::vars()
            .filter(|(key, _)| {
                !key.starts_with("ROOST_")
                    && !matches!(key.as_str(), "HOME" | "TMPDIR" | "TMP" | "TEMP")
            })
            .collect();
        for key in ["HOME", "TMPDIR", "TMP", "TEMP"] {
            env.push((key.to_string(), home.clone()));
        }
        env.push((MARKER_ENV.to_string(), self.marker.clone()));
        env
    }
}

/// The environment key that tags every process of one round.
pub const MARKER_ENV: &str = "ROOST_BENCH_RUN";
