//! Keeps this worker on the release its coordinator runs.
//!
//! The coordinator's `GET /version` names the release it is running; the
//! fleet's desired release is that exact tag, never GitHub's "latest", so a
//! rolled-back coordinator pulls its workers back too. When the names differ,
//! the poller starts `roost __self-update --to <tag>` in its own service unit
//! (see [`super::auto_update_spawn`]) and leaves the rest to that process: the
//! updater restarts this worker, and nothing here may be a child of it.
//!
//! [`decide`] is a pure function so the policy is testable without a
//! coordinator, a service manager, or a clock.

use std::path::{Path, PathBuf};
use std::time::Duration;

use roost_host::{EnvSource, ProcessEnv, build_identity, supported_host_platform};
use roost_platform::HostPlatform;
use roost_protocol::fleet_update::is_release_tag;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use super::auto_update_spawn::{forwarded_environment, spawn_updater};
use super::boot::WorkerBoot;

/// Two updater starts are at least this far apart, whatever the tag.
pub(crate) const RETRY_AFTER_MS: u64 = 60 * 60 * 1000;
/// The first poll waits this long, so a worker that is starting is not also
/// fetching.
const FIRST_POLL: Duration = Duration::from_secs(60);
const POLL_EVERY: Duration = Duration::from_secs(15 * 60);
const POLL_TIMEOUT: Duration = Duration::from_secs(20);
const ATTEMPT_FILE: &str = "self-update-attempt.json";
const OPT_OUT_VARIABLE: &str = "ROOST_AUTO_UPDATE";

/// The last updater this worker started, persisted so a restart of the worker
/// does not start a second one inside the retry window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub tag: String,
    pub at_ms: u64,
    /// The transient systemd unit that ran it (Linux); empty on macOS, where the
    /// detached child has no unit name.
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    UpToDate,
    Skip(&'static str),
    Update { tag: String },
}

/// Everything [`decide`] reads, gathered by the caller.
pub struct Facts<'a> {
    pub running: &'a str,
    pub desired: &'a str,
    pub last_attempt: Option<&'a Attempt>,
    pub now_ms: u64,
}

/// The update policy. Downgrades are updates too: the fleet follows the
/// coordinator, wherever it is.
pub fn decide(facts: &Facts<'_>) -> Decision {
    if !is_release_tag(facts.desired) {
        return Decision::Skip("the coordinator reports no release tag");
    }
    if facts.desired == facts.running {
        return Decision::UpToDate;
    }
    if let Some(last) = facts.last_attempt
        && last.at_ms <= facts.now_ms
        && facts.now_ms - last.at_ms < RETRY_AFTER_MS
    {
        return Decision::Skip("an update was started within the last hour");
    }
    Decision::Update {
        tag: facts.desired.to_owned(),
    }
}

/// `ROOST_AUTO_UPDATE=0|false|off` turns the poller off.
pub fn opted_out(value: Option<&str>) -> bool {
    matches!(
        value
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0" | "false" | "off")
    )
}

/// Why the poller must not run at all, decided once at startup: a source build
/// has no release to follow, and an opt-out is the operator's word. `None` when
/// the poller runs.
fn poller_off(is_compiled: bool, opted_out: bool) -> Option<&'static str> {
    if !is_compiled {
        Some("this worker is a source build")
    } else if opted_out {
        Some("ROOST_AUTO_UPDATE turns it off")
    } else {
        None
    }
}

/// Everything the poller needs, resolved once at startup.
struct Site {
    coordinator_base: String,
    data_dir: PathBuf,
    log_dir: PathBuf,
    platform: HostPlatform,
    running: String,
}

/// Start the poller, or `None` when this worker never updates itself: a
/// Windows host, a source build, an opt-out, or nothing to compare against.
/// Those checks run here so a dev build never starts a background task.
pub fn spawn_poller(boot: &WorkerBoot) -> Option<JoinHandle<()>> {
    let platform = match supported_host_platform() {
        Ok(platform @ (HostPlatform::Linux | HostPlatform::MacOs)) => platform,
        _ => {
            tracing::info!("worker self-update is for Linux and macOS workers; not polling");
            return None;
        }
    };
    let env = ProcessEnv::new();
    let identity = build_identity(&env);
    let opted_out = opted_out(env.get(OPT_OUT_VARIABLE).as_deref());
    if let Some(reason) = poller_off(identity.is_compiled, opted_out) {
        tracing::info!(reason, "worker self-update is off; not polling");
        return None;
    }
    let site = Site {
        coordinator_base: boot.coordinator_base.clone(),
        data_dir: boot.data_dir.clone(),
        log_dir: boot.log_dir.clone(),
        platform,
        running: identity.artifact_version,
    };
    Some(tokio::spawn(run(site)))
}

async fn run(site: Site) {
    let client = match reqwest::Client::builder().timeout(POLL_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "worker self-update is off: the HTTP client could not be built");
            return;
        }
    };
    tokio::time::sleep(FIRST_POLL).await;
    loop {
        poll_once(&client, &site).await;
        let jitter = Duration::from_millis(crate::capture::now_ms() % 60_000);
        tokio::time::sleep(POLL_EVERY + jitter).await;
    }
}

#[derive(Deserialize)]
struct VersionBody {
    version: String,
}

async fn fetch_version(client: &reqwest::Client, base: &str) -> Result<String, reqwest::Error> {
    let url = format!("{}/version", base.trim_end_matches('/'));
    let body: VersionBody = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body.version)
}

async fn poll_once(client: &reqwest::Client, site: &Site) {
    let desired = match fetch_version(client, &site.coordinator_base).await {
        Ok(version) => version,
        Err(error) => {
            tracing::warn!(%error, "could not read the coordinator's version; will try again");
            return;
        }
    };
    let last = read_attempt(&site.data_dir);
    let now_ms = crate::capture::now_ms();
    let facts = Facts {
        running: &site.running,
        desired: &desired,
        last_attempt: last.as_ref(),
        now_ms,
    };
    match decide(&facts) {
        Decision::UpToDate => {
            tracing::debug!(version = %desired, "worker runs the coordinator's release");
        }
        Decision::Skip(reason) => {
            tracing::debug!(version = %desired, reason, "not updating the worker");
        }
        Decision::Update { tag } => start_update(site, &tag, now_ms).await,
    }
}

async fn start_update(site: &Site, tag: &str, now_ms: u64) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::warn!(%error, "cannot find this worker's binary; not updating it");
            return;
        }
    };
    let started = tokio::task::spawn_blocking({
        let (platform, tag, log_dir) = (site.platform, tag.to_owned(), site.log_dir.clone());
        let forwarded = forwarded_environment();
        move || spawn_updater(platform, &exe, &tag, &log_dir, now_ms, &forwarded)
    })
    .await;
    let unit = match started {
        Ok(Ok(unit)) => unit,
        Ok(Err(error)) => {
            tracing::warn!(%error, tag, "could not start the worker's self-update; will try again");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, tag, "could not start the worker's self-update; will try again");
            return;
        }
    };
    let attempt = Attempt {
        tag: tag.to_owned(),
        at_ms: now_ms,
        unit,
    };
    if let Err(error) = write_attempt(&site.data_dir, &attempt) {
        tracing::warn!(%error, "could not record the worker's update attempt");
    }
    tracing::info!(tag, "started the worker's self-update");
}

fn read_attempt(data_dir: &Path) -> Option<Attempt> {
    let text = std::fs::read_to_string(data_dir.join(ATTEMPT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_attempt(data_dir: &Path, attempt: &Attempt) -> std::io::Result<()> {
    let text = serde_json::to_string(attempt).unwrap_or_default();
    let temporary = data_dir.join(format!("{ATTEMPT_FILE}.tmp"));
    std::fs::write(&temporary, text)?;
    std::fs::rename(temporary, data_dir.join(ATTEMPT_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = "v3.0.0-rc.36";
    const DESIRED: &str = "v3.0.0-rc.37";

    fn facts<'a>(desired: &'a str, last_attempt: Option<&'a Attempt>, now_ms: u64) -> Facts<'a> {
        Facts {
            running: RUNNING,
            desired,
            last_attempt,
            now_ms,
        }
    }

    fn attempt(at_ms: u64) -> Attempt {
        Attempt {
            tag: DESIRED.to_owned(),
            at_ms,
            unit: "unit".to_owned(),
        }
    }

    #[test]
    fn a_different_release_tag_starts_an_update_to_that_tag() {
        assert_eq!(
            decide(&facts(DESIRED, None, 1_000)),
            Decision::Update {
                tag: DESIRED.to_owned()
            }
        );
    }

    #[test]
    fn an_older_coordinator_release_is_followed_down() {
        assert_eq!(
            decide(&Facts {
                running: DESIRED,
                ..facts("v3.0.0-rc.36", None, 1_000)
            }),
            Decision::Update {
                tag: "v3.0.0-rc.36".to_owned()
            }
        );
    }

    #[test]
    fn the_same_release_is_up_to_date() {
        assert_eq!(decide(&facts(RUNNING, None, 1_000)), Decision::UpToDate);
    }

    #[test]
    fn a_source_build_or_an_opt_out_turns_the_poller_off() {
        assert_eq!(
            poller_off(false, false),
            Some("this worker is a source build")
        );
        assert_eq!(
            poller_off(false, true),
            Some("this worker is a source build")
        );
        assert_eq!(
            poller_off(true, true),
            Some("ROOST_AUTO_UPDATE turns it off")
        );
        assert_eq!(poller_off(true, false), None);
    }

    #[test]
    fn a_coordinator_without_a_release_tag_is_not_followed() {
        for desired in ["dev", "", "v", "3f2a9c1d0b8e7a6f5e4d3c2b1a0f9e8d7c6b5a49"] {
            assert!(
                matches!(decide(&facts(desired, None, 1_000)), Decision::Skip(_)),
                "{desired:?} must not be a release"
            );
        }
    }

    #[test]
    fn an_attempt_inside_the_retry_window_blocks_another_for_any_tag() {
        let last = attempt(1_000);
        let later = 1_000 + RETRY_AFTER_MS - 1;
        assert!(matches!(
            decide(&facts("v3.0.0-rc.38", Some(&last), later)),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn an_attempt_outside_the_retry_window_allows_another() {
        let last = attempt(1_000);
        let later = 1_000 + RETRY_AFTER_MS;
        assert!(matches!(
            decide(&facts(DESIRED, Some(&last), later)),
            Decision::Update { .. }
        ));
    }

    #[test]
    fn an_attempt_recorded_in_the_future_does_not_block_forever() {
        let last = attempt(9_000_000);
        assert!(matches!(
            decide(&facts(DESIRED, Some(&last), 1_000)),
            Decision::Update { .. }
        ));
    }

    #[test]
    fn the_opt_out_values_are_zero_false_and_off() {
        assert!(opted_out(Some("0")));
        assert!(opted_out(Some(" OFF ")));
        assert!(opted_out(Some("false")));
        assert!(!opted_out(Some("1")));
        assert!(!opted_out(None));
    }
}
