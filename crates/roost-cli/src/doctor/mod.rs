//! `roost doctor [--since <window>] [--session <sid>]` — the anomaly digest a
//! daily review reads. Owns the arguments, the cutoff, and the exit code;
//! doctor/log_sources.rs reads, doctor/digest.rs accumulates,
//! doctor/audit.rs adds what the coordinator already recorded about failed
//! calls, and doctor/digest_render.rs prints.
//!
//! Exit 0 means nothing to review and 1 means read the page above it. A
//! malformed `--since` is 2, because that is a usage error and a cron wrapper
//! needs to tell it apart from an alarming window. See
//! docs/phase6-cli-contract.md for the full list.
//!
//! Health is NOT this command's job: services, coordinator, front door and
//! workers are `roost status`. This one is logs and the request log only,
//! because a digest that also guessed at health would have to re-derive
//! diagnoses the coordinator and worker already made.

pub mod audit;
pub mod digest;
pub mod digest_render;
pub mod log_sources;
pub mod session_timeline;
pub mod window;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_host::{EnvSource, ProcessEnv};
use serde_json::Value;

use crate::command_error::CommandFailure;
use crate::doctor::audit::{
    AuditError, AuditSummary, read_audit_failures, read_audit_failures_postgres,
};
use crate::doctor::digest::{Digest, classify};
use crate::doctor::digest_render::render_digest;
use crate::doctor::log_sources::{LogSource, for_each_log_line, log_files_for, sources};
use crate::doctor::window::{DEFAULT_WINDOW_LABEL, parse_window};
use crate::status::collect::coordinator_database_path;
use crate::status::collect::installed_coordinator_environment;
use crate::status::service_definition::declared_value;
use crate::wall_clock;

#[derive(Debug, Args)]
#[command(about = "Anomaly digest from this host's logs and the coordinator's audit log")]
pub struct DoctorArgs {
    /// How far back to look: a number and a unit, e.g. 24h, 7d, 90m. The unit
    /// is required — an unbounded window on a command meant to be pasted into
    /// a daily review is how a week of logs silently becomes a day.
    #[arg(long, value_name = "WINDOW", default_value = DEFAULT_WINDOW_LABEL)]
    pub since: String,
    /// One session's timeline instead of the digest. Reads the `diag()`
    /// firehose as well, which the digest does not.
    #[arg(long, value_name = "SESSION_ID")]
    pub session: Option<String>,
}

pub async fn run(args: &DoctorArgs) -> Result<ExitCode, CommandFailure> {
    let window = parse_window(&args.since)?;
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let now_ms = wall_clock::now_ms();
    let cutoff_ms = now_ms - window.millis;
    let log_sources = sources(&env, platform);

    if let Some(session) = args.session.as_deref() {
        let rows = session_timeline::collect(&log_sources, session, cutoff_ms);
        println!("{}", session_timeline::render(session, &rows));
        // A timeline is a lookup, not a gate: "no events for this session" is
        // the most useful thing it can say, and it must not fail a script.
        return Ok(ExitCode::SUCCESS);
    }

    let (digest, missing) = accumulate(&log_sources, cutoff_ms);
    let audit = read_audit(&env, platform, cutoff_ms).await;
    let rendered = render_digest(
        &digest,
        &window.label,
        cutoff_ms,
        &missing,
        &audit,
        &host_name(&env),
    );
    println!("{}", rendered.text);
    Ok(if rendered.exit_code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Fold every source's files into one digest, recording which sources had no
/// file at all. A source with no file is reported, not treated as quiet: "the
/// worker has never run here" and "the worker had nothing to say" are opposite
/// facts and the digest says which one it found.
fn accumulate(log_sources: &[LogSource], cutoff_ms: i64) -> (Digest, Vec<String>) {
    let mut digest = Digest::new();
    let mut missing = Vec::new();
    for source in log_sources {
        let files = log_files_for(source);
        if files.is_empty() {
            missing.push(format!("{} ({})", source.app, source.dir.display()));
            continue;
        }
        for file in files {
            for_each_log_line(&file, &mut |line: Value| {
                classify(&mut digest, &line, cutoff_ms, &source.app);
            });
        }
    }
    (digest, missing)
}

/// The audit half. A coordinator that has never run has no audit log and
/// nothing to review, so every failure here degrades to an empty section. A
/// database that is absent (a worker-only host) is logged quietly; one that is
/// there and unreadable is a warning, which a CLI run prints, so an empty
/// section that should not be empty says why.
async fn read_audit(
    env: &impl EnvSource,
    platform: roost_host::HostPlatform,
    cutoff_ms: i64,
) -> AuditSummary {
    let read = match audit_source(env, platform) {
        None => return AuditSummary::default(),
        Some(AuditSource::Sqlite(path)) => read_audit_failures(&path, cutoff_ms).await,
        Some(AuditSource::Postgres(url)) => read_audit_failures_postgres(&url, cutoff_ms).await,
    };
    match read {
        Ok(summary) => summary,
        Err(error @ AuditError::Missing(_)) => {
            tracing::info!(target: "doctor", msg = "audit_absent", fields = error.to_string());
            AuditSummary::default()
        }
        Err(error) => {
            tracing::warn!(target: "doctor", msg = "audit_unreadable", fields = error.to_string());
            AuditSummary::default()
        }
    }
}

/// Where the coordinator's audit log lives.
#[derive(Debug, PartialEq, Eq)]
enum AuditSource {
    Sqlite(PathBuf),
    Postgres(String),
}

/// The coordinator's database: a Postgres URL the installed unit or this
/// process's environment declares (the coordinator in a container has one),
/// else the SQLite file `roost status` resolves.
fn audit_source(env: &impl EnvSource, platform: roost_host::HostPlatform) -> Option<AuditSource> {
    let installed = installed_coordinator_environment(env, platform);
    let url = declared_value(&installed, roost_host::ENV_COORDINATOR_DATABASE_URL)
        .map(str::to_owned)
        .or_else(|| env.get(roost_host::ENV_COORDINATOR_DATABASE_URL))
        .filter(|url| !url.is_empty() && roost_host::DatabaseLocation::is_postgres_url(url));
    if let Some(url) = url {
        return Some(AuditSource::Postgres(url));
    }
    coordinator_database_path(env, platform, &installed)
        .ok()
        .map(AuditSource::Sqlite)
}

/// `HOST` when the environment declares one, else `local` — the same fallback
/// the v2 digest used, kept so a pasted digest reads the same in v3.
fn host_name(env: &impl EnvSource) -> String {
    env.get("HOST")
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_string())
}

#[cfg(test)]
mod tests {
    use roost_host::{HOME_ENV, HostPlatform, MapEnv};

    use super::{AuditSource, audit_source};

    #[test]
    fn a_declared_postgres_url_is_the_audit_source() {
        let env = MapEnv::new()
            .with(HOME_ENV, "/nonexistent-roost-home")
            .with(
                roost_host::ENV_COORDINATOR_DATABASE_URL,
                "postgres://u@h/db",
            );
        assert_eq!(
            audit_source(&env, HostPlatform::Linux),
            Some(AuditSource::Postgres("postgres://u@h/db".to_owned()))
        );
    }

    #[test]
    fn without_a_url_the_audit_source_is_the_sqlite_file() {
        let env = MapEnv::new().with(HOME_ENV, "/nonexistent-roost-home");
        assert!(matches!(
            audit_source(&env, HostPlatform::Linux),
            Some(AuditSource::Sqlite(_))
        ));
    }
}
