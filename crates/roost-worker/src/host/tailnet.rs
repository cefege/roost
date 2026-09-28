//! This host's Tailscale MagicDNS name, read from `tailscale status --json`
//! without depending on a service manager's often-minimal `PATH`. Ports v2
//! `packages/host/src/tailnet.ts` (`tailscaleBinaryCandidates`,
//! `resolveTailnetDnsName`); `runtime::heartbeat_sources` re-resolves it for
//! the heartbeat's `reachable_addr`. Depends on `host::tool_path`.

use std::time::Duration;

use roost_host::{EnvSource, HostPlatform};
use serde_json::Value;

use super::tool_path::run_bounded;

/// An explicit `tailscale` binary, tried before the documented install paths.
pub const TAILSCALE_BIN_ENV: &str = "ROOST_TAILSCALE_BIN";

/// v2 bounds each `tailscale status --json` at two seconds (then SIGKILL).
const TAILSCALE_STATUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Where `tailscale` may be, in the order v2 tries them.
pub fn tailscale_binary_candidates(platform: HostPlatform, env: &dyn EnvSource) -> Vec<String> {
    let mut candidates: Vec<String> = env
        .get(TAILSCALE_BIN_ENV)
        .filter(|explicit| !explicit.is_empty())
        .into_iter()
        .collect();
    let installed: &[&str] = match platform {
        HostPlatform::MacOs => &[
            "tailscale",
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/opt/homebrew/bin/tailscale",
            "/usr/local/bin/tailscale",
        ],
        HostPlatform::Linux => &[
            "tailscale",
            "/usr/bin/tailscale",
            "/usr/local/bin/tailscale",
        ],
        // Windows is paused; its installed paths are not searched.
        HostPlatform::Windows => &[],
    };
    candidates.extend(installed.iter().map(|candidate| (*candidate).to_string()));
    candidates
}

/// The first candidate's `Self.DNSName`, lowercased and without its trailing
/// dot, or the empty string when no candidate answers with one.
pub fn resolve_tailnet_dns_name(candidates: &[String]) -> String {
    for candidate in candidates {
        let Some(out) = run_bounded(
            candidate,
            &["status", "--json"],
            None,
            None,
            TAILSCALE_STATUS_TIMEOUT,
        ) else {
            continue;
        };
        let dns_name = parse_self_dns_name(&out);
        if !dns_name.is_empty() {
            return dns_name;
        }
    }
    String::new()
}

/// `Self.DNSName` of a `tailscale status --json` document, normalized; empty
/// when the document does not parse or names none.
#[must_use]
pub fn parse_self_dns_name(status_json: &str) -> String {
    let Ok(status) = serde_json::from_str::<Value>(status_json) else {
        return String::new();
    };
    let name = status
        .get("Self")
        .and_then(|own| own.get("DNSName"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    name.strip_suffix('.').unwrap_or(&name).to_string()
}
