//! `roost-bench run`: preflight, then for each round each stack in turn —
//! boot, pair a fresh Chromium, drive every scenario under a sampler bracket,
//! stop — and finally write the report. Called by `main.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::browser::{BenchBrowser, pair_browser};
use crate::cli::RunArgs;
use crate::error::BenchError;
use crate::exec::capture_stdout;
use crate::paths;
use crate::prepare::Prepared;
use crate::report::{MachineInfo, RoundRecord, RunReport, WrittenReport, write_report};
use crate::sampler::Sampler;
use crate::scenario::{
    FloodKind, Sample, SessionPage, cold_nav, echo_rtt, fanout, flood, startup_samples,
    wait_session_ready,
};
use crate::stack::{MARKER_ENV, RoundLayout, StackHandle, boot_stack};

/// The settle before the idle-RSS reading, once the session page is up.
const IDLE_SETTLE: Duration = Duration::from_secs(2);
/// The operator's door override, the same key in both clients
/// (`roost-client-core` `client::local::discovery`, v2 `localWorkerDiscovery.ts`).
const LOCAL_WORKER_ORIGIN_KEY: &str = "roost.localWorkerOrigin";
/// How long a fresh profile is given to open a first-visit dialog.
const FIRST_VISIT_DIALOG_SETTLE: Duration = Duration::from_millis(2500);

pub async fn run(args: &RunArgs) -> Result<WrittenReport, BenchError> {
    let mut prepared = Prepared::load()?;
    if let Some(chrome) = &args.chrome {
        prepared.chrome = chrome.clone();
    }
    preflight(&prepared, args.allow_stale).await?;
    let run_id = utc_run_id();
    let run_dir = args
        .out
        .clone()
        .unwrap_or_else(|| paths::runs_root().join(&run_id));
    std::fs::create_dir_all(&run_dir)
        .map_err(|error| BenchError::io(format!("creating {}", run_dir.display()), error))?;
    let machine = machine_info().await;
    tracing::info!(run_id = %run_id, run_dir = %run_dir.display(), rounds = args.rounds, stacks = ?args.stacks, "bench run start");

    let mut report = RunReport {
        run_id: run_id.clone(),
        command_line: std::env::args().collect::<Vec<_>>().join(" "),
        prepared: prepared.clone(),
        machine,
        rounds: Vec::new(),
    };
    for round in 1..=args.rounds {
        for &stack in &args.stacks {
            let layout = RoundLayout::create(&run_dir, &run_id, stack, round)?;
            let record = run_round(layout, &prepared).await?;
            report.rounds.push(record);
            // Rewritten after every round, so an aborted run still leaves data.
            write_report(&run_dir, &report)?;
        }
    }
    let written = write_report(&run_dir, &report)?;
    tracing::info!(report = %written.markdown_path.display(), "bench run done");
    Ok(written)
}

async fn run_round(layout: RoundLayout, prepared: &Prepared) -> Result<RoundRecord, BenchError> {
    let stack = layout.stack;
    tracing::info!(stack = stack.as_str(), round = layout.round, root = %layout.root.display(), "round start");
    let sampler = Sampler::start(&layout.marker, &layout.root)?;
    let startup = sampler.mark("startup");
    let handle = match boot_stack(layout, prepared).await {
        Ok(handle) => handle,
        Err(error) => {
            crate::stack::sweep_marked(&sampler, stack.as_str()).await;
            sampler.stop();
            return Err(error);
        }
    };
    let mut record = RoundRecord::new(stack, handle.layout.round);
    let session = open_session(&handle, &mut record).await;
    record.usage.push(sampler.finish(startup));
    if let Some(session_id) = &session {
        drive_session(&handle, prepared, &sampler, session_id, &mut record).await;
        if let Err(error) = handle.client.kill_session(session_id).await {
            tracing::warn!(%error, "session kill failed");
        }
    }
    handle.stop(&sampler).await;
    sampler.stop();
    tracing::info!(
        stack = stack.as_str(),
        round = record.round,
        errors = record.errors.len(),
        "round done"
    );
    Ok(record)
}

async fn open_session(handle: &StackHandle, record: &mut RoundRecord) -> Option<String> {
    let folder = handle.layout.home.to_string_lossy().into_owned();
    let started = Instant::now();
    match handle.client.spawn_shell(&handle.worker_fp, &folder).await {
        Ok(session_id) => {
            let spawn_ms = started.elapsed().as_secs_f64() * 1000.0;
            record
                .samples
                .extend(startup_samples(handle.timings, spawn_ms));
            Some(session_id)
        }
        Err(error) => {
            record.errors.insert("startup".into(), error.to_string());
            None
        }
    }
}

async fn drive_session(
    handle: &StackHandle,
    prepared: &Prepared,
    sampler: &Sampler,
    session_id: &str,
    record: &mut RoundRecord,
) {
    let layout = &handle.layout;
    let chrome_env = vec![
        (MARKER_ENV.to_string(), layout.marker.clone()),
        (
            "HOME".to_string(),
            layout.home.to_string_lossy().into_owned(),
        ),
    ];
    let browser =
        match BenchBrowser::launch(&prepared.chrome, &layout.chrome_profile, chrome_env).await {
            Ok(browser) => browser,
            Err(error) => {
                record.errors.insert("browser".into(), error.to_string());
                return;
            }
        };
    let session_url = format!("{}/s/{session_id}", layout.coord_url());
    match ready_session(handle, &browser, session_url).await {
        Ok(session) => drive_scenarios(&session, sampler, record).await,
        Err(error) => {
            record.errors.insert("setup".into(), error.to_string());
        }
    }
    browser.close().await;
}

async fn ready_session<'browser>(
    handle: &StackHandle,
    browser: &'browser BenchBrowser,
    session_url: String,
) -> Result<SessionPage<'browser>, BenchError> {
    let layout = &handle.layout;
    let token = handle
        .client
        .mint_bootstrap("browser", "bench-browser")
        .await?;
    let page = pair_browser(browser, layout.stack, &layout.coord_url(), &token).await?;
    // Both clients probe the default door port unless the operator override
    // names another; the default port belongs to this machine's installed
    // worker, so the round's own door is named the way an operator would.
    page.eval::<serde_json::Value>(&format!(
        "localStorage.setItem('{LOCAL_WORKER_ORIGIN_KEY}', '{}')",
        layout.door_origin()
    ))
    .await?;
    page.goto(&session_url).await?;
    wait_session_ready(&page).await?;
    page.dismiss_open_dialogs(FIRST_VISIT_DIALOG_SETTLE).await?;
    SessionPage::new(browser, page, session_url)
}

async fn drive_scenarios(session: &SessionPage<'_>, sampler: &Sampler, record: &mut RoundRecord) {
    measure("cold_nav", sampler, record, cold_nav(session)).await;
    if let Err(error) = session.install_nonce().await {
        record
            .errors
            .insert("install_nonce".into(), error.to_string());
        return;
    }
    tokio::time::sleep(IDLE_SETTLE).await;
    record.idle_rss_bytes = sampler.rss_now();
    measure("echo_rtt", sampler, record, echo_rtt(session)).await;
    measure(
        "flood_plain",
        sampler,
        record,
        flood(session, FloodKind::Plain),
    )
    .await;
    measure(
        "flood_styled",
        sampler,
        record,
        flood(session, FloodKind::Styled),
    )
    .await;
    measure("fanout", sampler, record, fanout(session)).await;
}

/// Run one scenario inside a sampler bracket; a failure is recorded, not fatal.
async fn measure(
    name: &'static str,
    sampler: &Sampler,
    record: &mut RoundRecord,
    scenario: impl Future<Output = Result<Vec<Sample>, BenchError>>,
) {
    let bracket = sampler.mark(name);
    let outcome = scenario.await;
    record.usage.push(sampler.finish(bracket));
    match outcome {
        Ok(samples) => {
            tracing::info!(
                scenario = name,
                stack = record.stack.as_str(),
                samples = samples.len(),
                "scenario done"
            );
            record.samples.extend(samples);
        }
        Err(error) => {
            tracing::warn!(scenario = name, stack = record.stack.as_str(), %error, "scenario failed");
            record.errors.insert(name.to_string(), error.to_string());
        }
    }
}

async fn preflight(prepared: &Prepared, allow_stale: bool) -> Result<(), BenchError> {
    let repo = paths::repo_root();
    let v3_head = capture_stdout("git", &["rev-parse", "HEAD"], &repo).await?;
    let v2_head = capture_stdout("git", &["rev-parse", "HEAD"], &prepared.v2_root).await?;
    if !allow_stale && (v3_head != prepared.v3_sha || v2_head != prepared.v2_sha) {
        return Err(BenchError::Preflight(format!(
            "prepare built v3 {} / v2 {}, but HEAD is v3 {v3_head} / v2 {v2_head}; \
             re-run `roost-bench prepare` or pass --allow-stale",
            prepared.v3_sha, prepared.v2_sha
        )));
    }
    for required in [
        paths::v3_roost(),
        paths::v3_keeper(),
        paths::v3_web_dist().join("index.html"),
    ] {
        require(&required)?;
    }
    require(&paths::v2_web_dist(&prepared.v2_root).join("index.html"))?;
    require(&prepared.chrome)?;
    let nproc = nproc();
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let one_minute: f64 = load
        .split_whitespace()
        .next()
        .and_then(|field| field.parse().ok())
        .unwrap_or(0.0);
    if one_minute > 2.0 * nproc as f64 {
        return Err(BenchError::Preflight(format!(
            "1-minute load average {one_minute} exceeds 2 × {nproc} CPUs; numbers from a loaded \
             machine are not comparable"
        )));
    }
    Ok(())
}

fn require(path: &Path) -> Result<(), BenchError> {
    if path.exists() {
        Ok(())
    } else {
        Err(BenchError::Preflight(format!(
            "{} is missing; run `roost-bench prepare`",
            path.display()
        )))
    }
}

fn nproc() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

async fn machine_info() -> MachineInfo {
    MachineInfo {
        nproc: nproc(),
        loadavg_at_start: std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .trim()
            .to_string(),
        kernel: capture_stdout("uname", &["-srm"], Path::new("/"))
            .await
            .unwrap_or_default(),
    }
}

/// `YYYYMMDD-HHMMSS` in UTC, from the civil-from-days algorithm.
fn utc_run_id() -> String {
    let seconds = crate::coord::now_ms() / 1000;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}
