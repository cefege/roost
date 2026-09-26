//! The two HTTP questions `roost status` asks a coordinator: does its
//! unauthenticated identity RPC answer, and does its own listener serve a page?
//! Called by status/collect.rs. One client for both, because building a TLS
//! client per probe would repeat root-certificate setup on a command an
//! operator runs on a bad day.
//!
//! The probes are `Option`-shaped on purpose. "I could not ask" and "I asked
//! and the answer was no" are different facts: the first is a broken front
//! door or a machine with no coordinator, the second is a coordinator that is
//! up and refusing. Collapsing them into one boolean is how a healthy install
//! reads as dead.

use std::time::Duration;

use reqwest::Client;
use serde_json::Value;

use crate::status::report::COORD_IDENTITY_PATH;

/// A front door, a tunnel, or a wedged socket must not hold the readout open.
/// This is the deadline the TypeScript probe used (`AbortSignal.timeout(5000)`).
pub const PROBE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityProbe {
    pub reachable: bool,
    /// The build the coordinator reported. A reachable coordinator that
    /// reports no SHA is still reachable — the SHA only positions it against
    /// the fleet, and refusing to call it reachable would make a coordinator
    /// that predates the field look dead.
    pub git_sha: Option<String>,
}

impl IdentityProbe {
    fn unanswered() -> Self {
        Self {
            reachable: false,
            git_sha: None,
        }
    }
}

#[derive(Debug)]
pub struct HttpProbe {
    client: Client,
}

impl HttpProbe {
    /// One client, one deadline, for the whole command.
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder().timeout(PROBE_DEADLINE).build()?,
        })
    }

    /// POST the unauthenticated identity RPC: the liveness contract every
    /// listener answers, whether reached directly or through a front door.
    /// It is POSTed because a Connect service has no GET surface — a GET would
    /// 404 on a perfectly healthy coordinator and read as silence.
    pub async fn coordinator_identity(&self, origin: Option<&str>) -> IdentityProbe {
        let Some(origin) = origin else {
            return IdentityProbe::unanswered();
        };
        let response = self
            .client
            .post(format!("{origin}{COORD_IDENTITY_PATH}"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;
        let Ok(response) = response else {
            return IdentityProbe::unanswered();
        };
        if !response.status().is_success() {
            return IdentityProbe::unanswered();
        }
        let Ok(body) = response.json::<Value>().await else {
            return IdentityProbe::unanswered();
        };
        // Connect's JSON codec emits protobuf JSON names, so field 2
        // `git_sha` arrives as `gitSha`. Reading the snake_case spelling here
        // would be a second, wrong guess at the wire.
        let git_sha = body
            .get("gitSha")
            .and_then(Value::as_str)
            .filter(|sha| !sha.is_empty())
            .map(str::to_string);
        match git_sha {
            Some(git_sha) => IdentityProbe {
                reachable: true,
                git_sha: Some(git_sha),
            },
            None => IdentityProbe::unanswered(),
        }
    }

    /// HEAD the coordinator's own root. A page request is the only authority
    /// on whether a build is being served: the responder picks disk-vs-embed
    /// once at boot, so a dist created after the coordinator started is not
    /// served however current the configuration looks.
    ///
    /// Loopback only, never the front door: the coordinator's SPA arm 404s a
    /// request it cannot see as on-host whenever Cloudflare Access is
    /// configured, so probing the public origin would call a healthy install
    /// missing.
    pub async fn spa_root(&self, coord_url: Option<&str>) -> Option<bool> {
        let coord_url = coord_url?;
        let response = self
            .client
            .head(format!("{coord_url}/"))
            .send()
            .await
            .ok()?;
        Some(response.status() == reqwest::StatusCode::OK)
    }
}
