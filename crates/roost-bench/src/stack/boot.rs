//! Starting and stopping one stack: spawn the coordinator, wait for its
//! listener, seed the harness device, mint a worker token, spawn the worker,
//! wait until it is routable — stamping each step — and on stop, signal both
//! and sweep every leftover process carrying the round's marker.

use std::fs::File;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::{Child, Command};

use crate::coord::{BenchDevice, CoordClient, seed_device};
use crate::error::BenchError;
use crate::exec::signal_pids;
use crate::prepare::Prepared;
use crate::sampler::Sampler;
use crate::stack::RoundLayout;
use crate::stack::spec::{ProcessSpec, coord_spec, worker_spec};

const COORD_LISTEN_DEADLINE: Duration = Duration::from_secs(30);
const SEED_DEADLINE: Duration = Duration::from_secs(10);
const WORKER_ROUTABLE_DEADLINE: Duration = Duration::from_secs(60);
const GRACEFUL_STOP: Duration = Duration::from_secs(5);
const LOG_TAIL_LINES: usize = 40;

/// Startup milestones, measured from the coordinator spawn and the worker spawn.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StartupTimings {
    pub coord_listen_ms: f64,
    pub worker_routable_ms: f64,
}

/// A running stack.
#[derive(Debug)]
pub struct StackHandle {
    pub layout: RoundLayout,
    pub client: CoordClient,
    pub worker_fp: String,
    pub timings: StartupTimings,
    children: Vec<(&'static str, Child)>,
}

pub async fn boot_stack(
    layout: RoundLayout,
    prepared: &Prepared,
) -> Result<StackHandle, BenchError> {
    let mut children = Vec::new();
    let booted = boot_children(&layout, prepared, &mut children).await;
    match booted {
        Ok((client, worker_fp, timings)) => Ok(StackHandle {
            layout,
            client,
            worker_fp,
            timings,
            children,
        }),
        Err(error) => {
            stop_children(&mut children).await;
            Err(error)
        }
    }
}

async fn boot_children(
    layout: &RoundLayout,
    prepared: &Prepared,
    children: &mut Vec<(&'static str, Child)>,
) -> Result<(CoordClient, String, StartupTimings), BenchError> {
    let stack = layout.stack.as_str();
    let coord = coord_spec(layout, prepared);
    let coord_started = Instant::now();
    children.push(("coord", spawn_logged(&coord, layout, &layout.coord_log)?));
    tracing::info!(stack, command = %coord.describe(), port = layout.coord_port, "coordinator spawned");
    wait_for_listener(layout, children).await?;
    let coord_listen_ms = elapsed_ms(coord_started);

    let device = BenchDevice::generate()?;
    seed_with_retry(layout, &device).await?;
    let client = CoordClient::new(layout.coord_url(), device);
    let token = client
        .mint_bootstrap("worker", &format!("bench-{stack}"))
        .await
        .map_err(|error| boot_failure(layout, &layout.coord_log, error.to_string()))?;

    let worker = worker_spec(layout, prepared, &token);
    let worker_started = Instant::now();
    children.push(("worker", spawn_logged(&worker, layout, &layout.worker_log)?));
    tracing::info!(stack, command = %worker.describe(), door = layout.door_port, "worker spawned");
    let worker_fp = wait_for_routable(layout, &client, children).await?;
    let timings = StartupTimings {
        coord_listen_ms,
        worker_routable_ms: elapsed_ms(worker_started),
    };
    tracing::info!(stack, worker_fp = %worker_fp, ?timings, "stack booted");
    Ok((client, worker_fp, timings))
}

impl StackHandle {
    /// Stop the worker, then the coordinator, then everything else the round
    /// started (keepers, shells, Chromium leftovers).
    pub async fn stop(mut self, sampler: &Sampler) {
        self.children.reverse();
        stop_children(&mut self.children).await;
        sweep_marked(sampler, self.layout.stack.as_str()).await;
    }
}

/// SIGTERM, then SIGKILL every round process still alive, and report survivors.
pub async fn sweep_marked(sampler: &Sampler, stack: &str) {
    let leftovers = sampler.live_pids();
    if leftovers.is_empty() {
        return;
    }
    tracing::info!(stack, pids = ?leftovers, "terminating leftover round processes");
    signal_pids("TERM", &leftovers).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    signal_pids("KILL", &sampler.live_pids()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let survivors = sampler.live_pids();
    if !survivors.is_empty() {
        tracing::warn!(stack, pids = ?survivors, "round processes survived SIGKILL");
    }
}

async fn stop_children(children: &mut [(&'static str, Child)]) {
    for (name, child) in children.iter_mut() {
        let Some(pid) = child.id() else {
            continue;
        };
        signal_pids("TERM", &[pid]).await;
        match tokio::time::timeout(GRACEFUL_STOP, child.wait()).await {
            Ok(_) => tracing::info!(child = *name, pid, "child stopped"),
            Err(_) => {
                tracing::warn!(child = *name, pid, "child ignored SIGTERM; killing");
                if let Err(error) = child.kill().await {
                    tracing::warn!(child = *name, %error, "SIGKILL failed");
                }
            }
        }
    }
}

fn spawn_logged(
    spec: &ProcessSpec,
    layout: &RoundLayout,
    log_path: &std::path::Path,
) -> Result<Child, BenchError> {
    let log = File::options()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| BenchError::io(format!("opening {}", log_path.display()), error))?;
    let log_err = log
        .try_clone()
        .map_err(|error| BenchError::io(format!("cloning {}", log_path.display()), error))?;
    Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(layout.base_env())
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| BenchError::io(format!("spawning `{}`", spec.describe()), error))
}

async fn wait_for_listener(
    layout: &RoundLayout,
    children: &mut [(&'static str, Child)],
) -> Result<(), BenchError> {
    let started = Instant::now();
    let address = format!("127.0.0.1:{}", layout.coord_port);
    while started.elapsed() < COORD_LISTEN_DEADLINE {
        if tokio::net::TcpStream::connect(&address).await.is_ok() {
            return Ok(());
        }
        ensure_running(layout, children)?;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(boot_failure(
        layout,
        &layout.coord_log,
        format!("no listener on {address} within {COORD_LISTEN_DEADLINE:?}"),
    ))
}

/// The coordinator creates its account during boot; the listener can be up
/// a moment before that row is committed.
async fn seed_with_retry(layout: &RoundLayout, device: &BenchDevice) -> Result<(), BenchError> {
    let started = Instant::now();
    loop {
        match seed_device(&layout.coord_db, device).await {
            Ok(()) => return Ok(()),
            Err(error) if started.elapsed() >= SEED_DEADLINE => {
                return Err(boot_failure(layout, &layout.coord_log, error.to_string()));
            }
            Err(error) => tracing::debug!(%error, "seed not ready; retrying"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_routable(
    layout: &RoundLayout,
    client: &CoordClient,
    children: &mut [(&'static str, Child)],
) -> Result<String, BenchError> {
    let started = Instant::now();
    let mut last_error = String::new();
    while started.elapsed() < WORKER_ROUTABLE_DEADLINE {
        match client.routable_workers().await {
            Ok(fps) => {
                if let Some(fp) = fps.into_iter().next() {
                    return Ok(fp);
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        ensure_running(layout, children)?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(boot_failure(
        layout,
        &layout.worker_log,
        format!("worker not routable within {WORKER_ROUTABLE_DEADLINE:?} {last_error}"),
    ))
}

fn ensure_running(
    layout: &RoundLayout,
    children: &mut [(&'static str, Child)],
) -> Result<(), BenchError> {
    for (name, child) in children.iter_mut() {
        if let Ok(Some(status)) = child.try_wait() {
            let log = if *name == "coord" {
                &layout.coord_log
            } else {
                &layout.worker_log
            };
            return Err(boot_failure(
                layout,
                log,
                format!("{name} exited: {status}"),
            ));
        }
    }
    Ok(())
}

fn boot_failure(layout: &RoundLayout, log: &std::path::Path, reason: String) -> BenchError {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n");
    BenchError::Boot {
        stack: layout.stack.as_str(),
        reason,
        log: log.to_path_buf(),
        tail,
    }
}

fn elapsed_ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}
