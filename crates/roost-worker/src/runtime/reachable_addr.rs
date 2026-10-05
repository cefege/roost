//! The address this machine is reached at, re-sent on every heartbeat: the live
//! tailnet MagicDNS name, else the `ROOST_REACHABLE_ADDR` fallback deploy sets,
//! cached five minutes. Ports v2 `currentReachableAddr`
//! (`apps/worker/src/transport/heartbeat.ts`). `heartbeat_sources` owns one;
//! the resolver and environment are injected so the cache rule is testable.

use std::sync::Arc;

use roost_host::{EnvSource, ProcessEnv};
use roost_observability::clock::EventClock;

use super::bootstrap_redeem::ENV_REACHABLE_ADDR;
use crate::host::tailnet::resolve_tailnet_dns_name;

/// v2 `REACHABLE_ADDR_TTL_MS`: a resolved name is reused for five minutes, so
/// the 30 s beat does not fork `tailscale status` every tick.
pub const REACHABLE_ADDR_TTL_MS: i64 = 5 * 60_000;

/// Answers this host's live tailnet name, or the empty string when none.
pub type TailnetNameResolver = Box<dyn Fn() -> String + Send>;

/// The cached reachable address. Never derived from the worker label: a label
/// is a Tailscale HostName, which does not resolve, and v2 shipped dead
/// `vnc://worker-*` links that way.
pub struct ReachableAddr {
    resolve_tailnet_name: TailnetNameResolver,
    env: Box<dyn EnvSource + Send>,
    clock: Arc<dyn EventClock>,
    cached: Option<(String, i64)>,
}

impl std::fmt::Debug for ReachableAddr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReachableAddr")
            .field("cached", &self.cached)
            .finish_non_exhaustive()
    }
}

impl ReachableAddr {
    pub fn new(
        resolve_tailnet_name: TailnetNameResolver,
        env: Box<dyn EnvSource + Send>,
        clock: Arc<dyn EventClock>,
    ) -> Self {
        Self {
            resolve_tailnet_name,
            env,
            clock,
            cached: None,
        }
    }

    /// The production resolver: `tailscale status --json` over `candidates`,
    /// falling back to the process environment.
    pub fn from_tailnet(candidates: Vec<String>, clock: Arc<dyn EventClock>) -> Self {
        Self::new(
            Box::new(move || resolve_tailnet_dns_name(&candidates)),
            Box::new(ProcessEnv::new()),
            clock,
        )
    }

    /// The address for this beat, or `None` when nothing resolved.
    pub fn current(&mut self) -> Option<String> {
        let now_ms = self.clock.now_epoch_ms();
        if let Some((name, at_ms)) = &self.cached
            && now_ms - at_ms < REACHABLE_ADDR_TTL_MS
        {
            return Some(name.clone());
        }
        let mut resolved = (self.resolve_tailnet_name)();
        if resolved.is_empty() {
            resolved = self
                .env
                .get(ENV_REACHABLE_ADDR)
                .map(|value| value.trim().to_owned())
                .unwrap_or_default();
        }
        // A transient empty (tailscale not up yet) must not pin an empty value
        // for five minutes, so only a real answer refreshes the cache.
        if resolved.is_empty() {
            tracing::debug!("no reachable address resolved for this beat");
            return None;
        }
        if self
            .cached
            .as_ref()
            .is_none_or(|(name, _)| *name != resolved)
        {
            tracing::info!(reachable_addr = %resolved, "the reachable address was resolved");
        }
        self.cached = Some((resolved.clone(), now_ms));
        Some(resolved)
    }
}
